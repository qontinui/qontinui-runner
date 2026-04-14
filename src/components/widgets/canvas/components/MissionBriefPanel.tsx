/**
 * MissionBriefPanel Component
 *
 * Canvas panel type "MissionBrief" — renders a live-updating mission overview
 * with full prompt, stage dependency graph, progress indicator, and current activity.
 *
 * Data schema:
 * {
 *   workflow_name: string,
 *   prompt: string,
 *   model: string,
 *   reflection: boolean,
 *   approval_gate: boolean,
 *   stages: [{ name, index, max_iterations }],
 *   total_stages: number,
 *   max_iterations: number,
 *   progress: {
 *     current_stage_index: number | null,
 *     current_iteration: number,
 *     phase: string | null,
 *     activity: string | null,
 *   }
 * }
 */

import { useMemo } from "react";
import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./types";

// ─── Types ───

interface StageInfo {
  name: string;
  index: number;
  /** `null` indicates an unlimited cap. */
  max_iterations: number | null;
}

interface ProgressInfo {
  current_stage_index: number | null;
  current_iteration: number;
  phase: string | null;
  activity: string | null;
}

// ─── Phase colors ───

const PHASE_COLORS: Record<string, { bg: string; text: string; border: string; dot: string }> = {
  setup: {
    bg: "bg-blue-500/10",
    text: "text-blue-400",
    border: "border-blue-500/30",
    dot: "bg-blue-500",
  },
  verification: {
    bg: "bg-green-500/10",
    text: "text-green-400",
    border: "border-green-500/30",
    dot: "bg-green-500",
  },
  agentic: {
    bg: "bg-amber-500/10",
    text: "text-amber-400",
    border: "border-amber-500/30",
    dot: "bg-amber-500",
  },
  completion: {
    bg: "bg-purple-500/10",
    text: "text-purple-400",
    border: "border-purple-500/30",
    dot: "bg-purple-500",
  },
  completed: {
    bg: "bg-emerald-500/10",
    text: "text-emerald-400",
    border: "border-emerald-500/30",
    dot: "bg-emerald-500",
  },
};

const DEFAULT_PHASE = {
  bg: "bg-zinc-500/10",
  text: "text-zinc-400",
  border: "border-zinc-500/30",
  dot: "bg-zinc-500",
};

// ─── Sub-components ───

/** Full prompt with scrollable container */
function PromptSection({ prompt }: { prompt: string }) {
  const lineCount = prompt.split("\n").length;
  const needsScroll = lineCount > 5;

  return (
    <div
      className={cn(
        "rounded-md border border-border/50 bg-muted/30 px-3 py-2 text-xs text-foreground/80 font-mono whitespace-pre-wrap",
        needsScroll && "max-h-[7.5rem] overflow-y-auto",
      )}
    >
      {prompt}
    </div>
  );
}

/** Compact stage flow for multi-stage workflows */
function StageFlow({
  stages,
  currentStageIndex,
}: {
  stages: StageInfo[];
  currentStageIndex: number | null;
}) {
  if (stages.length <= 1) return null;

  return (
    <div className="flex items-center gap-1 flex-wrap">
      {stages.map((stage, i) => {
        const isCurrent = currentStageIndex === stage.index;
        const isDone = currentStageIndex !== null && stage.index < currentStageIndex;
        const isPending = currentStageIndex === null || stage.index > currentStageIndex;

        return (
          <div key={stage.index} className="flex items-center gap-1">
            {i > 0 && (
              <svg width="12" height="8" className="text-zinc-600 shrink-0">
                <path
                  d="M0 4 L8 4 M6 1 L9 4 L6 7"
                  stroke="currentColor"
                  fill="none"
                  strokeWidth="1.5"
                />
              </svg>
            )}
            <span
              className={cn(
                "text-[11px] px-2 py-0.5 rounded-full border transition-colors",
                isCurrent && "bg-primary/20 border-primary/50 text-primary font-semibold",
                isDone && "bg-emerald-500/15 border-emerald-500/30 text-emerald-400",
                isPending && "border-border/50 text-muted-foreground",
              )}
              title={`${stage.name} — max ${stage.max_iterations ?? "∞"} iterations`}
            >
              {stage.name}
            </span>
          </div>
        );
      })}
    </div>
  );
}

/** Compact progress indicator */
function ProgressIndicator({
  stages,
  progress,
  maxIterations,
}: {
  stages: StageInfo[];
  progress: ProgressInfo;
  /** `null` = unlimited; the inner progress bar becomes indeterminate. */
  maxIterations: number | null;
}) {
  const phase = progress.phase ?? "setup";
  const colors = PHASE_COLORS[phase] ?? DEFAULT_PHASE;
  const stageMaxIter: number | null =
    progress.current_stage_index !== null && stages[progress.current_stage_index]
      ? stages[progress.current_stage_index].max_iterations
      : maxIterations;

  // Calculate iteration progress bar width. Uncapped → 0% (indeterminate).
  const iterProgress =
    stageMaxIter != null && stageMaxIter > 0
      ? Math.min((progress.current_iteration / stageMaxIter) * 100, 100)
      : 0;

  return (
    <div className="space-y-1.5">
      {/* Progress bar + labels */}
      <div className="flex items-center gap-3">
        {/* Phase badge */}
        <span
          className={cn(
            "text-[10px] px-2 py-0.5 rounded-full border font-semibold uppercase tracking-wider",
            colors.bg,
            colors.text,
            colors.border,
          )}
        >
          {phase}
        </span>

        {/* Stage indicator (multi-stage only) */}
        {stages.length > 1 && progress.current_stage_index !== null && (
          <span className="text-[11px] text-muted-foreground">
            Stage {progress.current_stage_index + 1}/{stages.length}
          </span>
        )}

        {/* Iteration indicator */}
        {progress.current_iteration > 0 && (
          <span className="text-[11px] text-muted-foreground">
            Iteration {progress.current_iteration}/{stageMaxIter}
          </span>
        )}
      </div>

      {/* Progress bar */}
      {progress.current_iteration > 0 && (
        <div className="h-1 rounded-full bg-muted/50 overflow-hidden">
          <div
            className={cn("h-full rounded-full transition-all duration-500", colors.dot)}
            style={{ width: `${iterProgress}%`, opacity: 0.7 }}
          />
        </div>
      )}
    </div>
  );
}

/** Activity description */
function ActivityLine({ activity, phase }: { activity: string; phase: string | null }) {
  const colors = PHASE_COLORS[phase ?? "setup"] ?? DEFAULT_PHASE;

  return (
    <div className={cn("flex items-start gap-2 rounded-md px-2.5 py-1.5 text-xs", colors.bg)}>
      <span className={cn("mt-0.5 h-1.5 w-1.5 rounded-full shrink-0 animate-pulse", colors.dot)} />
      <span className={cn("leading-relaxed", colors.text)}>{activity}</span>
    </div>
  );
}

// ─── Main Component ───

export function MissionBriefPanel({ data }: CanvasPanelComponentProps) {
  const workflowName = (data.workflow_name as string) ?? "Workflow";
  const prompt = (data.prompt as string) ?? "";
  const model = (data.model as string) ?? "default";
  const reflection = (data.reflection as boolean) ?? false;
  const approvalGate = (data.approval_gate as boolean) ?? false;
  const stages = useMemo(() => (data.stages as StageInfo[] | undefined) ?? [], [data.stages]);
  // Pull through nullable: server may emit null/undefined to signal unlimited.
  const maxIterations = (data.max_iterations as number | null | undefined) ?? null;
  const progress = useMemo(
    () =>
      (data.progress as ProgressInfo | undefined) ?? {
        current_stage_index: null,
        current_iteration: 0,
        phase: null,
        activity: null,
      },
    [data.progress],
  );

  // Compact tags line
  const tags: string[] = [];
  if (model !== "default") tags.push(model);
  if (reflection) tags.push("reflection");
  if (approvalGate) tags.push("approval gate");

  return (
    <div className="space-y-3">
      {/* Header: workflow name + tags */}
      <div className="flex items-center gap-2 flex-wrap">
        <h3 className="text-sm font-semibold text-foreground">{workflowName}</h3>
        {tags.map((tag) => (
          <span
            key={tag}
            className="text-[10px] px-1.5 py-0 rounded border border-border/50 text-muted-foreground"
          >
            {tag}
          </span>
        ))}
      </div>

      {/* Prompt */}
      {prompt && <PromptSection prompt={prompt} />}

      {/* Stage dependency flow */}
      <StageFlow stages={stages} currentStageIndex={progress.current_stage_index} />

      {/* Progress indicator */}
      {progress.phase && (
        <ProgressIndicator stages={stages} progress={progress} maxIterations={maxIterations} />
      )}

      {/* Current activity */}
      {progress.activity && <ActivityLine activity={progress.activity} phase={progress.phase} />}
    </div>
  );
}

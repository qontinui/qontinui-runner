/**
 * ExecutionTimelineSummary Component
 *
 * Compact summary view for the execution timeline widget.
 * Shows current phase, step count, and progress indicator.
 */

import { Loader2, CheckCircle2, XCircle, Clock } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge } from "../../../ui";
import { StepStatsBar } from "../shared";
import { getAccentColors, getStatusColors } from "@/design-system";
import { WORKFLOW_STAGE_CONFIG } from "../../../../types/dashboard/activity-types";
import type { ExecutionTimelineSummaryProps } from "./types";

/**
 * Compact phase progress indicator.
 */
function PhaseProgress({
  phaseGroups,
  currentPhase,
}: {
  phaseGroups: ExecutionTimelineSummaryProps["data"]["phaseGroups"];
  currentPhase: ExecutionTimelineSummaryProps["data"]["currentPhase"];
}) {
  return (
    <div className="flex items-center gap-1 mt-2">
      {phaseGroups.map((group) => {
        const config = WORKFLOW_STAGE_CONFIG[group.phase];
        const colors = getAccentColors(config.color as Parameters<typeof getAccentColors>[0]);
        const isCurrent = group.phase === currentPhase;
        const isComplete = group.isComplete;

        return (
          <div
            key={group.phase}
            className={cn(
              "flex-1 h-1.5 rounded-full transition-all",
              isComplete
                ? "bg-green-500"
                : isCurrent
                  ? cn(colors.bg, "animate-phase-fade")
                  : "bg-muted",
            )}
            title={`${config.label}: ${group.stats.completed}/${group.stats.total}`}
          />
        );
      })}
    </div>
  );
}

/**
 * Summary component for execution timeline.
 */
export function ExecutionTimelineSummary({ data, className }: ExecutionTimelineSummaryProps) {
  const { phaseGroups, stepStats, currentPhase, currentStep, isLoading } = data;
  const successColors = getStatusColors("success");
  const errorColors = getStatusColors("error");

  if (isLoading) {
    return (
      <div className={cn("flex items-center justify-center py-4", className)}>
        <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
      </div>
    );
  }

  // No steps yet
  if (stepStats.total === 0) {
    return (
      <div className={cn("text-center py-2", className)}>
        <Clock className="h-4 w-4 mx-auto mb-1 text-muted-foreground opacity-50" />
        <span className="text-xs text-muted-foreground">Waiting for execution...</span>
      </div>
    );
  }

  return (
    <div className={cn("space-y-2", className)}>
      {/* Compact stats */}
      <StepStatsBar stats={stepStats} compact />

      {/* Current step indicator */}
      {currentStep && (
        <div className="flex items-center gap-2 text-xs">
          <Loader2 className={cn(
            "h-3 w-3 animate-spin",
            currentStep.id === "_workflow_processing" ? "text-purple-500" : "text-blue-500"
          )} />
          <span className={cn(
            "truncate",
            currentStep.id === "_workflow_processing" ? "text-purple-500/70 italic" : "text-muted-foreground"
          )}>
            {currentStep.name}
          </span>
        </div>
      )}

      {/* Phase progress bar */}
      {phaseGroups.length > 0 && (
        <PhaseProgress phaseGroups={phaseGroups} currentPhase={currentPhase} />
      )}

      {/* Quick stats row */}
      <div className="flex items-center justify-between text-xs">
        {/* Current phase */}
        {currentPhase && (
          <Badge variant="muted" className="text-[10px]">
            {WORKFLOW_STAGE_CONFIG[currentPhase].label}
          </Badge>
        )}

        {/* Pass/fail counts */}
        <div className="flex items-center gap-2">
          {stepStats.successful > 0 && (
            <div className="flex items-center gap-1">
              <CheckCircle2 className={cn("h-3 w-3", successColors.text)} />
              <span className={successColors.text}>{stepStats.successful}</span>
            </div>
          )}
          {stepStats.failed > 0 && (
            <div className="flex items-center gap-1">
              <XCircle className={cn("h-3 w-3", errorColors.text)} />
              <span className={errorColors.text}>{stepStats.failed}</span>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export default ExecutionTimelineSummary;

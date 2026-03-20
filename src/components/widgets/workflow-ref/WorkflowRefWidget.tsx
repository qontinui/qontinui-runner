/**
 * WorkflowRefWidget Component
 *
 * Full widget view for sub-workflow execution activity.
 * Displays workflow list, progress, sub-steps, and status.
 */

import { useState } from "react";
import { GitBranch, ChevronRight, Layers } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge, ScrollArea, Progress } from "@/components/ui";
import { StepStatsBar, StepStatusBadge } from "../shared";
import { getAccentColors, getStatusColors } from "@/design-system";
import type { WorkflowRefWidgetProps } from "./types";
import type { WorkflowRefExecution, StepExecution } from "../shared/types";

/**
 * Workflow row component showing workflow details.
 */
function WorkflowRow({
  workflow,
  isSelected,
  onSelect,
}: {
  workflow: WorkflowRefExecution;
  isSelected: boolean;
  onSelect: () => void;
}) {
  const pinkColors = getAccentColors("pink");
  const errorColors = getStatusColors("error");
  const isActive = workflow.status === "running";
  const pendingColors = getStatusColors("pending");
  const progressPercent =
    workflow.totalSteps > 0 ? (workflow.completedSteps / workflow.totalSteps) * 100 : 0;

  return (
    <div
      className={cn(
        "flex items-start gap-3 border-l-2 px-4 py-2.5 transition-colors cursor-pointer",
        isActive
          ? cn(pendingColors.border, pendingColors.bg)
          : isSelected
            ? "border-primary bg-muted/50"
            : "border-transparent hover:bg-muted/30",
      )}
      onClick={onSelect}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect();
        }
      }}
      role="button"
      tabIndex={0}
    >
      {/* Status icon */}
      <StepStatusBadge status={workflow.status} iconOnly size="md" />

      {/* Workflow badge */}
      <Badge
        className={cn(
          "text-xs flex-shrink-0 font-mono border",
          pinkColors.bg,
          pinkColors.text,
          pinkColors.border,
        )}
      >
        <GitBranch className="h-3 w-3 mr-1" />
        SUB
      </Badge>

      {/* Workflow content */}
      <div className="flex-1 min-w-0">
        <p className="font-medium text-sm text-foreground truncate">{workflow.workflowName}</p>
        {workflow.totalSteps > 0 && (
          <div className="flex items-center gap-2 mt-1">
            <Progress value={progressPercent} className="h-1.5 flex-1" />
            <span className="text-xs text-muted-foreground font-mono">
              {workflow.completedSteps}/{workflow.totalSteps}
            </span>
          </div>
        )}
        {workflow.error && (
          <p className={cn("mt-0.5 text-xs truncate", errorColors.text)}>{workflow.error}</p>
        )}
      </div>

      {/* Duration */}
      {workflow.durationMs !== undefined && (
        <span className="font-mono text-xs text-muted-foreground flex-shrink-0">
          {workflow.durationMs < 1000
            ? `${workflow.durationMs}ms`
            : `${(workflow.durationMs / 1000).toFixed(1)}s`}
        </span>
      )}
    </div>
  );
}

/**
 * Sub-step row component.
 */
function SubStepRow({ step }: { step: StepExecution }) {
  const errorColors = getStatusColors("error");

  return (
    <div className="flex items-center gap-3 px-4 py-2 hover:bg-muted/30 transition-colors">
      <ChevronRight className="h-3 w-3 text-muted-foreground flex-shrink-0" />
      <StepStatusBadge status={step.status} iconOnly size="sm" />
      <span className="text-sm text-foreground truncate flex-1">{step.name}</span>
      {step.error && (
        <span className={cn("text-xs truncate max-w-[200px]", errorColors.text)}>{step.error}</span>
      )}
      {step.durationMs !== undefined && (
        <span className="font-mono text-xs text-muted-foreground flex-shrink-0">
          {step.durationMs < 1000
            ? `${step.durationMs}ms`
            : `${(step.durationMs / 1000).toFixed(1)}s`}
        </span>
      )}
    </div>
  );
}

/**
 * Workflow detail panel showing sub-steps.
 */
function WorkflowDetail({ workflow }: { workflow: WorkflowRefExecution | null }) {
  if (!workflow) {
    return (
      <div className="flex-1 flex items-center justify-center text-muted-foreground text-sm">
        Select a workflow to view details
      </div>
    );
  }

  const progressPercent =
    workflow.totalSteps > 0 ? (workflow.completedSteps / workflow.totalSteps) * 100 : 0;

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      {/* Workflow header */}
      <div className="px-4 py-3 border-b border-border bg-muted/10 flex-shrink-0">
        <div className="flex items-center gap-2">
          <GitBranch className="h-4 w-4 text-muted-foreground" />
          <span className="font-medium text-sm text-foreground">{workflow.workflowName}</span>
          <StepStatusBadge status={workflow.status} size="sm" />
        </div>
        {workflow.totalSteps > 0 && (
          <div className="flex items-center gap-3 mt-2">
            <Progress value={progressPercent} className="h-2 flex-1" />
            <span className="text-sm text-muted-foreground font-mono">
              {workflow.completedSteps} / {workflow.totalSteps} steps
            </span>
          </div>
        )}
      </div>

      {/* Sub-steps list */}
      <ScrollArea className="flex-1">
        <div className="py-2">
          {workflow.subSteps && workflow.subSteps.length > 0 ? (
            workflow.subSteps.map((step) => <SubStepRow key={step.id} step={step} />)
          ) : (
            <div className="flex flex-col items-center justify-center h-32 text-muted-foreground text-sm">
              <Layers className="h-8 w-8 mb-2 opacity-50" />
              {workflow.status === "running" ? (
                <p>Workflow is running...</p>
              ) : (
                <p>No sub-step details available</p>
              )}
            </div>
          )}
        </div>
      </ScrollArea>
    </div>
  );
}

/**
 * Full Workflow Ref widget component.
 */
export function WorkflowRefWidget({ isSummary, data, className }: WorkflowRefWidgetProps) {
  const [selectedWorkflowId, setSelectedWorkflowId] = useState<string | null>(null);

  if (isSummary) {
    return null;
  }

  const selectedWorkflow = data.workflows.find((w) => w.id === selectedWorkflowId) || null;

  return (
    <div className={cn("flex flex-col h-full overflow-hidden", className)}>
      {/* Stats Bar */}
      <StepStatsBar stats={data.stats} />

      {/* Main content - split view */}
      <div className="flex-1 flex overflow-hidden">
        {/* Workflow list */}
        <div className="w-1/2 border-r border-border flex flex-col">
          <div className="flex items-center justify-between border-b border-border px-4 py-2 bg-muted/10 flex-shrink-0">
            <h3 className="text-sm font-semibold text-foreground">Sub-Workflows</h3>
            <Badge variant="muted" className="text-xs">
              {data.workflows.length}
            </Badge>
          </div>
          <ScrollArea className="flex-1">
            <div className="flex flex-col-reverse">
              {data.workflows.slice(0, 50).map((workflow) => (
                <WorkflowRow
                  key={workflow.id}
                  workflow={workflow}
                  isSelected={workflow.id === selectedWorkflowId}
                  onSelect={() => setSelectedWorkflowId(workflow.id)}
                />
              ))}
            </div>
            {data.workflows.length === 0 && (
              <div className="flex items-center justify-center h-24 text-muted-foreground text-sm">
                No sub-workflows executed yet
              </div>
            )}
          </ScrollArea>
        </div>

        {/* Workflow detail */}
        <div className="w-1/2 flex flex-col">
          <WorkflowDetail workflow={selectedWorkflow} />
        </div>
      </div>
    </div>
  );
}

export default WorkflowRefWidget;

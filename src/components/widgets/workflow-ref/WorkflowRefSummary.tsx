/**
 * WorkflowRefSummary Component
 *
 * Compact summary view for workflow ref widget.
 * Shows quick stats and the last few workflows with progress.
 */

import { GitBranch, Loader2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { Badge, Progress } from "@/components/ui";
import { StepStatsBar, StepStatusBadge } from "../shared";
import { getStatusColors, getAccentColors } from "@/design-system";
import type { WorkflowRefSummaryProps } from "./types";
import type { WorkflowRefExecution } from "../shared/types";

/**
 * Compact workflow row for summary view.
 */
function CompactWorkflowRow({ workflow }: { workflow: WorkflowRefExecution }) {
  const pinkColors = getAccentColors("pink");
  const progressPercent =
    workflow.totalSteps > 0 ? (workflow.completedSteps / workflow.totalSteps) * 100 : 0;

  return (
    <div className="flex items-center gap-2 py-1">
      <StepStatusBadge status={workflow.status} iconOnly size="sm" />
      <Badge
        className={cn(
          "text-[10px] px-1.5 py-0 font-mono border",
          pinkColors.bg,
          pinkColors.text,
          pinkColors.border,
        )}
      >
        <GitBranch className="h-2.5 w-2.5 mr-0.5" />
        SUB
      </Badge>
      <span className="text-xs text-muted-foreground truncate flex-1">{workflow.workflowName}</span>
      {workflow.totalSteps > 0 && (
        <div className="flex items-center gap-1.5 shrink-0">
          <Progress value={progressPercent} className="h-1 w-12" />
          <span className="text-[10px] text-muted-foreground font-mono">
            {workflow.completedSteps}/{workflow.totalSteps}
          </span>
        </div>
      )}
    </div>
  );
}

/**
 * Compact summary widget for workflow refs.
 */
export function WorkflowRefSummary({ status, data }: WorkflowRefSummaryProps) {
  const { workflows, currentWorkflow, stats } = data;
  const recentWorkflows = workflows.slice(0, 3);
  const pendingColors = getStatusColors("pending");

  return (
    <div className="space-y-3">
      {/* Quick Stats */}
      <StepStatsBar stats={stats} compact />

      {/* Recent Workflows */}
      {recentWorkflows.length > 0 ? (
        <div className="space-y-0.5">
          {recentWorkflows.map((workflow) => (
            <CompactWorkflowRow key={workflow.id} workflow={workflow} />
          ))}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">No sub-workflows executed yet</p>
      )}

      {/* Running Indicator */}
      {status === "running" && currentWorkflow && (
        <div className="flex items-center gap-2 pt-1 border-t border-border/50">
          <Loader2 className={cn("h-3 w-3 animate-spin", pendingColors.text)} />
          <span className={cn("text-xs truncate", pendingColors.text)}>
            {currentWorkflow.workflowName}
          </span>
          {currentWorkflow.totalSteps > 0 && (
            <span className="text-[10px] text-muted-foreground font-mono">
              ({currentWorkflow.completedSteps}/{currentWorkflow.totalSteps})
            </span>
          )}
        </div>
      )}
    </div>
  );
}

export default WorkflowRefSummary;

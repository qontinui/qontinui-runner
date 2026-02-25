/**
 * UiBridgeSummary Component
 *
 * Compact summary view for the UI Bridge widget.
 * Shows quick stats with assertion pass/fail counts and recent actions.
 */

import { Monitor, Loader2, Navigation, Play, CheckCircle2, Camera } from "lucide-react";
import { cn } from "../../../../lib/utils";
import { Badge } from "../../../ui";
import { StepStatsBar, StepStatusBadge } from "../shared";
import { getStatusColors, getAccentColors } from "@/design-system";
import type { UiBridgeSummaryProps, UiBridgeExecution, UiBridgeActionType } from "./types";

function getActionDisplay(actionType: UiBridgeActionType) {
  switch (actionType) {
    case "navigate":
      return { icon: Navigation, label: "NAV", color: "blue" };
    case "execute":
      return { icon: Play, label: "EXEC", color: "emerald" };
    case "assert":
      return { icon: CheckCircle2, label: "ASRT", color: "teal" };
    case "snapshot":
      return { icon: Camera, label: "SNAP", color: "purple" };
    default:
      return { icon: Monitor, label: "ACT", color: "slate" };
  }
}

function CompactActionRow({ execution }: { execution: UiBridgeExecution }) {
  const actionDisplay = getActionDisplay(execution.actionType);
  const actionColors = getAccentColors(actionDisplay.color);
  const ActionIcon = actionDisplay.icon;

  return (
    <div className="flex items-center gap-2 py-1">
      <StepStatusBadge status={execution.status} iconOnly size="sm" />
      <Badge
        className={cn(
          "text-[10px] px-1.5 py-0 border",
          actionColors.bg,
          actionColors.text,
          actionColors.border,
        )}
      >
        <ActionIcon className="h-2.5 w-2.5 mr-0.5" />
        {actionDisplay.label}
      </Badge>
      <span className="text-xs text-muted-foreground truncate flex-1">{execution.name}</span>
    </div>
  );
}

export function UiBridgeSummary({ status, data }: UiBridgeSummaryProps) {
  const { executions, currentExecution, stats, assertionStats } = data;
  const recentExecutions = executions.slice(0, 3);
  const pendingColors = getStatusColors("pending");

  return (
    <div className="space-y-3">
      <StepStatsBar stats={stats} compact />

      {/* Assertion summary */}
      {assertionStats.total > 0 && (
        <div className="flex items-center gap-2">
          <span className="text-[10px] text-muted-foreground">Assertions:</span>
          <Badge variant="success" className="text-[10px] px-1">
            {assertionStats.passed}
          </Badge>
          {assertionStats.failed > 0 && (
            <Badge variant="danger" className="text-[10px] px-1">
              {assertionStats.failed}
            </Badge>
          )}
        </div>
      )}

      {recentExecutions.length > 0 ? (
        <div className="space-y-0.5">
          {recentExecutions.map((execution) => (
            <CompactActionRow key={execution.id} execution={execution} />
          ))}
        </div>
      ) : (
        <p className="text-xs text-muted-foreground">No UI Bridge actions yet</p>
      )}

      {status === "running" && currentExecution && (
        <div className="flex items-center gap-2 pt-1 border-t border-border/50">
          <Loader2 className={cn("h-3 w-3 animate-spin", pendingColors.text)} />
          <span className={cn("text-xs truncate", pendingColors.text)}>
            {currentExecution.name}
          </span>
        </div>
      )}
    </div>
  );
}

export default UiBridgeSummary;

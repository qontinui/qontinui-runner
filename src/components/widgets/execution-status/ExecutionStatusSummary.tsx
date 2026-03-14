/**
 * ExecutionStatusSummary Component
 *
 * Compact summary widget for the Active Dashboard sidebar.
 * Shows session budget bar, current stage indicator, and pass rate trend.
 */

import { Circle, Activity, Zap, TrendingUp, Layers } from "lucide-react";
import { Badge } from "@/components/ui/Badge";
import { getAccentColors } from "@/design-system";
import { getStatusInfo, getSessionBudgetColor, getPassRate, formatElapsedTime } from "./utils";
import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import type { ExecutionStatusWidgetData } from "./types";

interface ExecutionStatusSummaryProps extends BaseWidgetProps {
  data: ExecutionStatusWidgetData;
}

export function ExecutionStatusSummary({ data }: ExecutionStatusSummaryProps) {
  const { sessionBudget, stageProgress, iterationTrend, status, timing } = data;
  const statusInfo = getStatusInfo(status);

  if (status === "idle" && sessionBudget.sessionsUsed === 0) {
    return (
      <div className="text-center py-2">
        <Activity className="h-4 w-4 mx-auto mb-1 text-muted-foreground opacity-50" />
        <span className="text-xs text-muted-foreground">No active workflow</span>
      </div>
    );
  }

  const budgetColor = getSessionBudgetColor(sessionBudget.sessionsUsed, sessionBudget.maxSessions);
  const budgetColors = getAccentColors(budgetColor);
  const budgetPct = sessionBudget.maxSessions
    ? Math.min((sessionBudget.sessionsUsed / sessionBudget.maxSessions) * 100, 100)
    : 0;

  const latestResult =
    iterationTrend.results.length > 0
      ? iterationTrend.results[iterationTrend.results.length - 1]
      : null;
  const latestPassRate = latestResult ? getPassRate(latestResult.passed, latestResult.total) : 0;

  return (
    <div className="space-y-2.5">
      {/* Status + Iteration */}
      <div className="flex items-center justify-between">
        <Badge className={`${statusInfo.color.bg} ${statusInfo.color.text}`} size="sm">
          <Circle className={`w-2 h-2 mr-1.5 ${statusInfo.dotClass}`} />
          {statusInfo.label}
        </Badge>
        {timing.elapsedSeconds > 0 && (
          <span className="text-[10px] text-muted-foreground font-mono">
            {formatElapsedTime(timing.elapsedSeconds)}
          </span>
        )}
      </div>

      {/* Session Budget Bar */}
      <div className="space-y-1">
        <div className="flex items-center justify-between text-[11px]">
          <div className="flex items-center gap-1">
            <Zap className="w-3 h-3 text-muted-foreground" />
            <span className="text-muted-foreground">Sessions</span>
          </div>
          <span className={`font-mono ${budgetColors.text}`}>
            {sessionBudget.sessionsUsed}
            {sessionBudget.maxSessions !== null ? ` / ${sessionBudget.maxSessions}` : ""}
          </span>
        </div>
        {sessionBudget.maxSessions !== null && (
          <div className="w-full h-1.5 bg-muted/50 rounded-full overflow-hidden">
            <div
              className={`h-full rounded-full transition-all duration-500 ${budgetColors.bgSolid}`}
              style={{ width: `${budgetPct}%` }}
            />
          </div>
        )}
      </div>

      {/* Stage indicator (multi-stage) */}
      {stageProgress && (
        <div className="flex items-center gap-1.5 text-[11px]">
          <Layers className="w-3 h-3 text-muted-foreground" />
          <span className="text-muted-foreground">Stage</span>
          <span className="text-foreground">
            {stageProgress.currentStageIndex + 1}/{stageProgress.totalStages}
          </span>
          {stageProgress.currentStageName && (
            <span className="text-muted-foreground truncate max-w-[80px]">
              {stageProgress.currentStageName}
            </span>
          )}
        </div>
      )}

      {/* Pass Rate Trend */}
      {iterationTrend.results.length > 0 && (
        <div className="space-y-1">
          <div className="flex items-center justify-between text-[11px]">
            <div className="flex items-center gap-1">
              <TrendingUp className="w-3 h-3 text-muted-foreground" />
              <span className="text-muted-foreground">Pass rate</span>
            </div>
            <span
              className={
                latestPassRate === 100
                  ? "text-green-500"
                  : latestPassRate === 0
                    ? "text-red-500"
                    : "text-amber-500"
              }
            >
              {Math.round(latestPassRate)}%
            </span>
          </div>
          {/* Mini sparkline */}
          <div className="flex items-end gap-px h-3">
            {iterationTrend.results.map((result) => {
              const passRate = getPassRate(result.passed, result.total);
              const isAllPassed = result.passed === result.total && result.total > 0;
              const isNonePassed = result.passed === 0 && result.total > 0;
              const barColor = isAllPassed
                ? "bg-green-500"
                : isNonePassed
                  ? "bg-red-500"
                  : "bg-amber-500";
              return (
                <div
                  key={result.iteration}
                  className={`flex-1 rounded-t-sm ${barColor} opacity-80`}
                  style={{ height: `${Math.max(passRate * 0.12, 1)}px`, minHeight: "1px" }}
                />
              );
            })}
          </div>
        </div>
      )}

      {/* Iteration counter */}
      {iterationTrend.currentIteration > 0 && (
        <div className="text-[10px] text-muted-foreground text-center pt-1 border-t border-border/30">
          Iteration {iterationTrend.currentIteration}
          {iterationTrend.maxIterations > 0 ? ` / ${iterationTrend.maxIterations}` : ""}
        </div>
      )}
    </div>
  );
}

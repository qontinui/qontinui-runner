/**
 * ExecutionStatusFullWidget Component
 *
 * Full-size widget for displaying workflow health in the Active Dashboard.
 * Shows session budget, stage progress, iteration trend, current activity,
 * and elapsed time with ETA.
 */

import { Circle, Radio, Activity, Clock, Layers, Zap, TrendingUp, Timer } from "lucide-react";
import { Badge } from "@/components/ui/Badge";
import { ScrollArea } from "@/components/ui/ScrollArea";
import { EmptyState } from "../shared";
import { getAccentColors } from "@/design-system";
import { getStatusInfo, formatElapsedTime, getSessionBudgetColor, getPassRate } from "./utils";
import type { BaseWidgetProps } from "@/types/dashboard/widget-props";
import type { ExecutionStatusWidgetData, IterationResult } from "./types";

interface ExecutionStatusFullWidgetProps extends BaseWidgetProps {
  data: ExecutionStatusWidgetData;
}

/**
 * Session budget progress bar with color thresholds.
 */
function SessionBudgetSection({
  sessionsUsed,
  maxSessions,
}: {
  sessionsUsed: number;
  maxSessions: number | null;
}) {
  const budgetColor = getSessionBudgetColor(sessionsUsed, maxSessions);
  const colors = getAccentColors(budgetColor);
  const pct = maxSessions ? Math.min((sessionsUsed / maxSessions) * 100, 100) : 0;

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Zap className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">Session Budget</span>
        </div>
        <span
          className={`text-sm font-mono ${maxSessions !== null ? colors.text : "text-muted-foreground"}`}
        >
          {sessionsUsed} {maxSessions !== null ? `/ ${maxSessions}` : ""} session
          {sessionsUsed !== 1 ? "s" : ""}
        </span>
      </div>
      {maxSessions !== null && (
        <div className="w-full h-2 bg-muted/50 rounded-full overflow-hidden">
          <div
            className={`h-full rounded-full transition-all duration-500 ${colors.bgSolid}`}
            style={{ width: `${pct}%` }}
          />
        </div>
      )}
    </div>
  );
}

/**
 * Stage progress pipeline for multi-stage workflows.
 */
function StageProgressSection({
  stageProgress,
}: {
  stageProgress: NonNullable<ExecutionStatusWidgetData["stageProgress"]>;
}) {
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <Layers className="w-4 h-4 text-muted-foreground" />
        <span className="text-sm font-medium">Stage Progress</span>
        <Badge variant="muted" size="sm">
          {stageProgress.currentStageIndex + 1} / {stageProgress.totalStages}
        </Badge>
      </div>
      <div className="flex items-center gap-1.5">
        {stageProgress.stageResults.map((stage, i) => (
          <div key={stage.index} className="flex items-center gap-1.5">
            {i > 0 && <div className="w-4 h-px bg-border" />}
            <div className="flex flex-col items-center gap-1">
              <div
                className={`w-6 h-6 rounded-full flex items-center justify-center text-[10px] font-medium ${
                  stage.status === "completed"
                    ? "bg-green-500/20 text-green-500"
                    : stage.status === "running"
                      ? "bg-blue-500/20 text-blue-400 ring-2 ring-blue-500/30"
                      : stage.status === "failed"
                        ? "bg-red-500/20 text-red-500"
                        : "bg-muted/50 text-muted-foreground"
                }`}
                title={stage.name}
              >
                {stage.index + 1}
              </div>
              <span className="text-[9px] text-muted-foreground truncate max-w-[60px]">
                {stage.name}
              </span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/**
 * Compact sparkline bar chart showing iteration verification pass rates.
 */
function IterationTrendSection({
  results,
  currentIteration,
  maxIterations,
}: {
  results: IterationResult[];
  currentIteration: number;
  maxIterations: number;
}) {
  if (results.length === 0 && currentIteration === 0) {
    return (
      <div className="space-y-2">
        <div className="flex items-center gap-2">
          <TrendingUp className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">Iteration Trend</span>
        </div>
        <span className="text-xs text-muted-foreground">No verification results yet</span>
      </div>
    );
  }

  const latestResult = results.length > 0 ? results[results.length - 1] : null;
  const latestPassRate = latestResult ? getPassRate(latestResult.passed, latestResult.total) : 0;

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <TrendingUp className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">Iteration Trend</span>
        </div>
        <div className="flex items-center gap-2">
          {latestResult && (
            <span className="text-xs text-muted-foreground">
              {latestResult.passed}/{latestResult.total} passing
            </span>
          )}
          <Badge variant="muted" size="sm">
            {currentIteration} / {maxIterations}
          </Badge>
        </div>
      </div>

      {/* Sparkline bar chart */}
      <div className="flex items-end gap-1 h-8">
        {results.map((result) => {
          const passRate = getPassRate(result.passed, result.total);
          const isAllPassed = result.passed === result.total && result.total > 0;
          const isNonePassed = result.passed === 0 && result.total > 0;
          const barColor = isAllPassed
            ? "bg-green-500"
            : isNonePassed
              ? "bg-red-500"
              : "bg-amber-500";
          const isCurrent = result.iteration === currentIteration;

          return (
            <div
              key={result.iteration}
              className="flex-1 flex flex-col items-center gap-0.5"
              title={`Iteration ${result.iteration}: ${result.passed}/${result.total} passed (${Math.round(passRate)}%)`}
            >
              <div
                className={`w-full rounded-t-sm transition-all duration-300 ${barColor} ${
                  isCurrent ? "opacity-100" : "opacity-70"
                }`}
                style={{
                  height: `${Math.max(passRate * 0.32, 2)}px`,
                  minHeight: "2px",
                }}
              />
            </div>
          );
        })}
        {/* Placeholder bars for remaining iterations */}
        {Array.from({ length: Math.max(0, maxIterations - results.length) }).map((_, i) => (
          <div key={`placeholder-${i}`} className="flex-1">
            <div className="w-full h-[2px] rounded-t-sm bg-muted/30" />
          </div>
        ))}
      </div>

      {/* Pass rate trend indicator */}
      {results.length >= 2 && (
        <div className="flex items-center gap-1 text-[10px]">
          {(() => {
            const prev = results[results.length - 2];
            const prevRate = getPassRate(prev.passed, prev.total);
            const delta = latestPassRate - prevRate;
            if (Math.abs(delta) < 0.1) {
              return <span className="text-muted-foreground">Pass rate stable</span>;
            }
            return (
              <span className={delta > 0 ? "text-green-500" : "text-red-500"}>
                {delta > 0 ? "+" : ""}
                {Math.round(delta)}% from previous
              </span>
            );
          })()}
        </div>
      )}
    </div>
  );
}

/**
 * Current activity and elapsed time display.
 */
function ActivityAndTimingSection({
  currentActivity,
  elapsedSeconds,
  etaSeconds,
  status,
}: {
  currentActivity: string;
  elapsedSeconds: number;
  etaSeconds: number | null;
  status: ExecutionStatusWidgetData["status"];
}) {
  return (
    <div className="space-y-3">
      {/* Current Activity */}
      <div className="space-y-1">
        <div className="flex items-center gap-2">
          <Activity className="w-4 h-4 text-muted-foreground" />
          <span className="text-sm font-medium">Current Activity</span>
        </div>
        <p className="text-sm text-foreground pl-6">{currentActivity}</p>
      </div>

      {/* Elapsed Time & ETA */}
      {(elapsedSeconds > 0 || status === "running") && (
        <div className="flex items-center gap-4 pl-6">
          <div className="flex items-center gap-1.5">
            <Timer className="w-3.5 h-3.5 text-muted-foreground" />
            <span className="text-xs text-muted-foreground">
              Elapsed:{" "}
              <span className="text-foreground font-mono">{formatElapsedTime(elapsedSeconds)}</span>
            </span>
          </div>
          {etaSeconds !== null && status === "running" && (
            <div className="flex items-center gap-1.5">
              <Clock className="w-3.5 h-3.5 text-muted-foreground" />
              <span className="text-xs text-muted-foreground">
                ETA:{" "}
                <span className="text-foreground font-mono">~{formatElapsedTime(etaSeconds)}</span>
              </span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export function ExecutionStatusFullWidget({ data }: ExecutionStatusFullWidgetProps) {
  const {
    sessionBudget,
    stageProgress,
    iterationTrend,
    currentActivity,
    timing,
    status,
    workflowName,
    isConnected,
  } = data;

  const statusInfo = getStatusInfo(status);
  const hasAnyData = status !== "idle" || sessionBudget.sessionsUsed > 0;

  if (!hasAnyData) {
    return (
      <div className="h-full">
        <EmptyState
          icon={Activity}
          title="No active workflow"
          description="Workflow health and progress will appear here when a workflow is running"
        />
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="px-4 py-3 border-b border-border/50 bg-muted/20 shrink-0">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <Badge className={`${statusInfo.color.bg} ${statusInfo.color.text}`}>
              <Circle className={`w-2 h-2 mr-1.5 ${statusInfo.dotClass}`} />
              {statusInfo.label}
            </Badge>
            <div
              className="flex items-center gap-1.5"
              title={isConnected ? "Connected to events" : "Not connected"}
            >
              <Radio
                className={`w-3 h-3 ${isConnected ? "text-green-500" : "text-muted-foreground"}`}
              />
            </div>
            {workflowName && (
              <span className="text-sm text-muted-foreground truncate max-w-[200px]">
                {workflowName}
              </span>
            )}
          </div>
          {iterationTrend.currentIteration > 0 && (
            <span className="text-xs text-muted-foreground">
              Iteration {iterationTrend.currentIteration}
            </span>
          )}
        </div>
      </div>

      {/* Scrollable Content */}
      <ScrollArea className="flex-1">
        <div className="p-4 space-y-5">
          {/* Session Budget */}
          <SessionBudgetSection
            sessionsUsed={sessionBudget.sessionsUsed}
            maxSessions={sessionBudget.maxSessions}
          />

          {/* Stage Progress (multi-stage only) */}
          {stageProgress && (
            <>
              <div className="border-t border-border/30" />
              <StageProgressSection stageProgress={stageProgress} />
            </>
          )}

          {/* Iteration Trend */}
          <div className="border-t border-border/30" />
          <IterationTrendSection
            results={iterationTrend.results}
            currentIteration={iterationTrend.currentIteration}
            maxIterations={iterationTrend.maxIterations}
          />

          {/* Current Activity & Timing */}
          <div className="border-t border-border/30" />
          <ActivityAndTimingSection
            currentActivity={currentActivity}
            elapsedSeconds={timing.elapsedSeconds}
            etaSeconds={timing.etaSeconds}
            status={status}
          />
        </div>
      </ScrollArea>
    </div>
  );
}

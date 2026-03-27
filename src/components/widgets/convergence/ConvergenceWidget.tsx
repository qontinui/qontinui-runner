/**
 * ConvergenceWidget Component
 *
 * Dashboard widget that visualizes convergence of the verification-agentic loop.
 * Shows a bar chart of failed step counts per iteration, trend indicators,
 * and a compact summary of the current convergence state.
 */

import { useState, useEffect, useMemo } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { TrendingDown, TrendingUp, Minus, AlertTriangle } from "lucide-react";
import { cn } from "@/lib/utils";
import { useCurrentTaskRunId } from "@/contexts";

interface IterationMetric {
  iteration: number;
  failedStepCount: number;
  passedStepCount: number;
  skippedStepCount: number;
  newFailures: number;
  repeatedFailures: number;
  isStalled: boolean;
}

/** Maximum number of iterations to display in the chart */
const MAX_VISIBLE_ITERATIONS = 20;

/**
 * Determine the trend based on recent metrics.
 */
function getTrend(metrics: IterationMetric[]): "improving" | "stalled" | "regressing" | "none" {
  if (metrics.length === 0) return "none";
  const latest = metrics[metrics.length - 1];
  if (latest.isStalled) return "stalled";
  if (metrics.length < 2) return "none";
  const previous = metrics[metrics.length - 2];
  if (latest.failedStepCount < previous.failedStepCount) return "improving";
  if (latest.failedStepCount > previous.failedStepCount) return "regressing";
  return "stalled";
}

/**
 * Get bar color based on failed count and trend context.
 */
function getBarColor(metric: IterationMetric, prevMetric: IterationMetric | null): string {
  if (metric.failedStepCount === 0) return "bg-emerald-500";
  if (prevMetric && metric.failedStepCount < prevMetric.failedStepCount) return "bg-amber-500";
  if (prevMetric && metric.failedStepCount > prevMetric.failedStepCount) return "bg-red-500";
  return "bg-amber-500";
}

/**
 * TrendIndicator shows the current convergence trend with icon and label.
 */
function TrendIndicator({ trend }: { trend: ReturnType<typeof getTrend> }) {
  switch (trend) {
    case "improving":
      return (
        <div className="flex items-center gap-1 text-emerald-400 text-xs font-medium">
          <TrendingDown className="w-3.5 h-3.5" />
          <span>Improving</span>
        </div>
      );
    case "stalled":
      return (
        <div className="flex items-center gap-1 text-amber-400 text-xs font-medium">
          <Minus className="w-3.5 h-3.5" />
          <span>Stalled</span>
        </div>
      );
    case "regressing":
      return (
        <div className="flex items-center gap-1 text-red-400 text-xs font-medium">
          <TrendingUp className="w-3.5 h-3.5" />
          <span>Regressing</span>
        </div>
      );
    default:
      return null;
  }
}

/**
 * MiniBarChart renders a simple CSS-based bar chart of failed step counts.
 */
function MiniBarChart({ metrics }: { metrics: IterationMetric[] }) {
  const visibleMetrics = metrics.slice(-MAX_VISIBLE_ITERATIONS);
  const maxFailed = Math.max(1, ...visibleMetrics.map((m) => m.failedStepCount));
  const chartHeight = 80;

  return (
    <div className="flex items-end gap-0.5 h-[80px] px-1 overflow-x-auto">
      {visibleMetrics.map((metric, idx) => {
        const prevMetric = idx > 0 ? visibleMetrics[idx - 1] : null;
        const barHeight = Math.max(2, (metric.failedStepCount / maxFailed) * chartHeight);
        const barColor = getBarColor(metric, prevMetric);

        return (
          <div
            key={metric.iteration}
            className="flex flex-col items-center gap-0.5 min-w-[16px]"
            title={`Iteration ${metric.iteration}: ${metric.failedStepCount} failed, ${metric.passedStepCount} passed`}
          >
            <div
              className={cn("w-3 rounded-t transition-all duration-300", barColor)}
              style={{ height: `${barHeight}px` }}
            />
            <span className="text-[9px] text-zinc-500 leading-none">{metric.iteration}</span>
          </div>
        );
      })}
    </div>
  );
}

/**
 * ConvergenceWidget component.
 *
 * Listens for iteration-metrics events and displays a convergence dashboard
 * showing failed step count over time, trend, and summary.
 */
export function ConvergenceWidget() {
  const taskRunId = useCurrentTaskRunId();
  const [metrics, setMetrics] = useState<IterationMetric[]>([]);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    // Reset metrics when task run changes
    // eslint-disable-next-line react-hooks/set-state-in-effect -- subscription callback
    setMetrics([]);

    const setup = async () => {
      unlisten = await listen(
        "iteration-metrics",
        (event: { payload: Record<string, unknown> }) => {
          const data = event.payload;
          // The AppEvent is serialized with serde tag="event_type", content="data"
          const inner = (data as Record<string, unknown>).data as
            | Record<string, unknown>
            | undefined;
          const eventData = inner || data;
          const eventTaskRunId = eventData.task_run_id as string;
          if (taskRunId && eventTaskRunId !== taskRunId) return;

          setMetrics((prev) => [
            ...prev,
            {
              iteration: eventData.iteration as number,
              failedStepCount: eventData.failed_step_count as number,
              passedStepCount: eventData.passed_step_count as number,
              skippedStepCount: eventData.skipped_step_count as number,
              newFailures: eventData.new_failures as number,
              repeatedFailures: eventData.repeated_failures as number,
              isStalled: eventData.is_stalled as boolean,
            },
          ]);
        },
      );
    };

    setup();

    return () => {
      if (unlisten) unlisten();
    };
  }, [taskRunId]);

  const trend = useMemo(() => getTrend(metrics), [metrics]);
  const latest = metrics.length > 0 ? metrics[metrics.length - 1] : null;

  if (metrics.length === 0) {
    return null;
  }

  // Build summary text
  const summaryParts: string[] = [];
  if (latest) {
    summaryParts.push(`Iteration ${latest.iteration}`);
    summaryParts.push(`${latest.failedStepCount} failing`);
    if (metrics.length > 1) {
      const prev = metrics[metrics.length - 2];
      const delta = latest.failedStepCount - prev.failedStepCount;
      if (delta < 0) {
        summaryParts.push(`(down ${Math.abs(delta)} from ${prev.failedStepCount})`);
      } else if (delta > 0) {
        summaryParts.push(`(up ${delta} from ${prev.failedStepCount})`);
      }
    }
    if (latest.newFailures > 0) {
      summaryParts.push(`${latest.newFailures} new`);
    }
    if (latest.repeatedFailures > 0) {
      summaryParts.push(`${latest.repeatedFailures} repeated`);
    }
  }

  return (
    <div className="flex flex-col gap-2 p-3 bg-zinc-800/50 rounded-lg border border-zinc-700/50">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h3 className="text-xs font-semibold text-zinc-300 uppercase tracking-wider">
            Convergence
          </h3>
          {latest?.isStalled && <AlertTriangle className="w-3.5 h-3.5 text-amber-400" />}
        </div>
        <TrendIndicator trend={trend} />
      </div>

      {/* Bar chart */}
      <MiniBarChart metrics={metrics} />

      {/* Summary */}
      <div className="text-xs text-zinc-400">{summaryParts.join(" \u00b7 ")}</div>

      {/* Pass/fail counts for latest iteration */}
      {latest && (
        <div className="flex items-center gap-3 text-[10px]">
          <span className="text-emerald-400">{latest.passedStepCount} passed</span>
          <span className="text-red-400">{latest.failedStepCount} failed</span>
          {latest.skippedStepCount > 0 && (
            <span className="text-zinc-500">{latest.skippedStepCount} skipped</span>
          )}
        </div>
      )}
    </div>
  );
}

export default ConvergenceWidget;

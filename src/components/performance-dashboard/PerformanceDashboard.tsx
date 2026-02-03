/**
 * PerformanceDashboard.tsx
 *
 * Main dashboard layout showing:
 * - Summary metrics header
 * - Action timing chart
 * - Success rate trend
 * - Transition reliability table
 * - Element resolution metrics
 */

import { useState } from "react";
import {
  BarChart3,
  RefreshCw,
  AlertTriangle,
  Activity,
  Zap,
  CheckCircle,
  Clock,
  Image,
} from "lucide-react";
import { usePerformanceDashboard } from "../../hooks/usePerformanceMetrics";
import type { TimeRange } from "../../types/performance-metrics";
import { formatDurationMs, formatSuccessRate } from "../../types/performance-metrics";
import { MetricCard } from "./MetricCard";
import { TimeRangeSelector } from "./TimeRangeSelector";
import { ActionTimingChart } from "./ActionTimingChart";
import { SuccessRateTrend } from "./SuccessRateTrend";
import { TransitionReliabilityTable } from "./TransitionReliabilityTable";

interface PerformanceDashboardProps {
  /** Configuration ID to show metrics for */
  configId: string | null;
  /** Configuration name for display */
  configName?: string;
}

export function PerformanceDashboard({ configId, configName }: PerformanceDashboardProps) {
  const [timeRange, setTimeRange] = useState<TimeRange>("7d");

  const {
    data: dashboard,
    isLoading,
    error,
    refetch,
  } = usePerformanceDashboard(configId, timeRange);

  if (!configId) {
    return (
      <div className="h-full flex items-center justify-center text-muted-foreground">
        <div className="text-center">
          <BarChart3 className="w-12 h-12 mx-auto mb-4 opacity-50" />
          <p>Load a configuration to view performance metrics</p>
        </div>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="h-full flex items-center justify-center">
        <RefreshCw className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error || !dashboard) {
    return (
      <div className="h-full flex items-center justify-center text-muted-foreground">
        <div className="text-center">
          <AlertTriangle className="w-12 h-12 mx-auto mb-4 opacity-50" />
          <p>No performance data available</p>
          <p className="text-sm mt-2">Run some workflows to start collecting metrics</p>
        </div>
      </div>
    );
  }

  const { summary, action_metrics, transition_metrics, element_metrics, success_rate_trend } =
    dashboard;

  // Determine success rate variant
  const getSuccessVariant = (rate: number): "success" | "warning" | "error" | "default" => {
    if (rate >= 0.95) return "success";
    if (rate >= 0.8) return "warning";
    if (rate >= 0.5) return "warning";
    return "error";
  };

  return (
    <div className="h-full overflow-auto p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold flex items-center gap-2">
            <BarChart3 className="w-5 h-5" />
            Performance Metrics
          </h2>
          <p className="text-sm text-muted-foreground">{configName || configId}</p>
        </div>
        <div className="flex items-center gap-3">
          <TimeRangeSelector value={timeRange} onChange={setTimeRange} />
          <button
            onClick={() => refetch()}
            className="p-2 rounded-md hover:bg-muted transition-colors"
            title="Refresh metrics"
          >
            <RefreshCw className="w-4 h-4" />
          </button>
        </div>
      </div>

      {/* Summary Cards */}
      <div className="grid grid-cols-2 md:grid-cols-4 lg:grid-cols-6 gap-3">
        <MetricCard
          title="Success Rate"
          value={formatSuccessRate(summary.overall_success_rate)}
          subtitle={`${summary.total_runs} total runs`}
          icon={CheckCircle}
          variant={getSuccessVariant(summary.overall_success_rate)}
        />
        <MetricCard
          title="Avg Duration"
          value={formatDurationMs(summary.avg_run_duration_ms)}
          icon={Clock}
        />
        <MetricCard
          title="Total Actions"
          value={summary.total_actions.toLocaleString()}
          icon={Zap}
        />
        <MetricCard
          title="Total Runs"
          value={summary.total_runs}
          subtitle={
            summary.last_run_at
              ? `Last: ${new Date(summary.last_run_at).toLocaleDateString()}`
              : undefined
          }
          icon={Activity}
        />
        <MetricCard
          title="Flaky Transitions"
          value={summary.flaky_transition_count}
          icon={AlertTriangle}
          variant={summary.flaky_transition_count > 0 ? "warning" : "default"}
        />
        <MetricCard
          title="Flaky Templates"
          value={summary.flaky_template_count}
          icon={Image}
          variant={summary.flaky_template_count > 0 ? "warning" : "default"}
        />
      </div>

      {/* Charts Row */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <ActionTimingChart data={action_metrics} maxItems={8} />
        <SuccessRateTrend data={success_rate_trend} timeRange={timeRange} />
      </div>

      {/* Tables Row */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <TransitionReliabilityTable data={transition_metrics} maxItems={10} />
        <ElementResolutionPanel data={element_metrics} maxItems={10} />
      </div>
    </div>
  );
}

// =============================================================================
// Element Resolution Panel (inline component)
// =============================================================================

interface ElementResolutionPanelProps {
  data: Array<{
    element_name: string;
    total_attempts: number;
    successful_matches: number;
    failed_matches: number;
    success_rate: number;
    avg_confidence: number;
    is_flaky: boolean;
  }>;
  maxItems?: number;
}

function ElementResolutionPanel({ data, maxItems = 0 }: ElementResolutionPanelProps) {
  // Sort by flakiness and success rate
  const sortedData = [...data].sort((a, b) => {
    if (a.is_flaky && !b.is_flaky) return -1;
    if (!a.is_flaky && b.is_flaky) return 1;
    return a.success_rate - b.success_rate;
  });

  const displayData = maxItems > 0 ? sortedData.slice(0, maxItems) : sortedData;
  const flakyCount = displayData.filter((d) => d.is_flaky).length;

  if (displayData.length === 0) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <h3 className="font-semibold mb-4 flex items-center gap-2">
          <Image className="w-4 h-4" />
          Element Resolution
        </h3>
        <div className="text-center text-muted-foreground py-8">
          <p>No element resolution data available</p>
          <p className="text-xs mt-1">Template matching results will appear here</p>
        </div>
      </div>
    );
  }

  const getSuccessRateColor = (rate: number, isFlaky: boolean) => {
    if (isFlaky) return "text-orange-500";
    if (rate >= 0.95) return "text-green-500";
    if (rate >= 0.8) return "text-yellow-500";
    if (rate >= 0.5) return "text-orange-500";
    return "text-red-500";
  };

  const getConfidenceColor = (confidence: number) => {
    if (confidence >= 0.9) return "text-green-500";
    if (confidence >= 0.8) return "text-yellow-500";
    return "text-orange-500";
  };

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <Image className="w-4 h-4" />
        Element Resolution
        {flakyCount > 0 && (
          <span className="text-xs bg-orange-500/10 text-orange-500 px-2 py-0.5 rounded-full ml-2">
            {flakyCount} flaky
          </span>
        )}
      </h3>

      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-border text-left">
              <th className="pb-2 font-medium text-muted-foreground">Element</th>
              <th className="pb-2 font-medium text-muted-foreground text-right">Match Rate</th>
              <th className="pb-2 font-medium text-muted-foreground text-right">Confidence</th>
              <th className="pb-2 font-medium text-muted-foreground text-right">Attempts</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {displayData.map((element, idx) => (
              <tr
                key={`${element.element_name}-${idx}`}
                className={`hover:bg-muted/30 ${element.is_flaky ? "bg-orange-500/5" : ""}`}
              >
                <td className="py-2">
                  <div className="flex items-center gap-2">
                    <span className="font-medium truncate max-w-40" title={element.element_name}>
                      {element.element_name}
                    </span>
                    {element.is_flaky && (
                      <span className="text-xs bg-orange-500/20 text-orange-500 px-1 rounded">
                        flaky
                      </span>
                    )}
                  </div>
                </td>
                <td
                  className={`py-2 text-right font-medium ${getSuccessRateColor(element.success_rate, element.is_flaky)}`}
                >
                  {formatSuccessRate(element.success_rate)}
                </td>
                <td
                  className={`py-2 text-right font-medium ${getConfidenceColor(element.avg_confidence)}`}
                >
                  {(element.avg_confidence * 100).toFixed(0)}%
                </td>
                <td className="py-2 text-right text-muted-foreground">{element.total_attempts}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {maxItems > 0 && data.length > maxItems && (
        <p className="text-xs text-muted-foreground mt-2 text-center">
          Showing {maxItems} of {data.length} elements
        </p>
      )}
    </div>
  );
}

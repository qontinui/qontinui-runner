import { useState } from "react";
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  AreaChart,
  Area,
} from "recharts";
import { TrendingUp, ChevronDown, ChevronRight } from "lucide-react";
import { useWorkflowTrends, useComponentTrend } from "@/hooks/useArchitecture";
import { EffectivenessTrendWidget } from "./EffectivenessTrendWidget";
import type { TrendPoint } from "@/types/architecture";
import type { TimeRange } from "@/types/performance-metrics";
import { TIME_RANGE_LABELS } from "@/types/performance-metrics";

interface TrendsPanelProps {
  workflowName: string;
  selectedComponent: string | null;
}

const TREND_TIME_RANGES: TimeRange[] = ["7d", "30d", "all"];

function formatTimestamp(ts: string, range: TimeRange): string {
  const d = new Date(ts);
  if (range === "1h" || range === "24h") {
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }
  return d.toLocaleDateString([], { month: "short", day: "numeric" });
}

interface MiniChartProps {
  title: string;
  data: TrendPoint[];
  color: string;
  timeRange: TimeRange;
  isPercentage?: boolean;
  isArea?: boolean;
}

function ChartTooltip({
  active,
  payload,
  isPercentage,
}: {
  active?: boolean;
  payload?: { payload: { displayTime: string; value: number; count?: number } }[];
  isPercentage?: boolean;
}) {
  if (active && payload && payload.length) {
    const d = payload[0].payload;
    return (
      <div className="bg-popover border border-border rounded-lg p-2 shadow-lg text-xs">
        <p className="font-medium">{d.displayTime}</p>
        <p className="text-muted-foreground">
          {isPercentage ? `${(d.value * 100).toFixed(1)}%` : d.value.toFixed(2)}
        </p>
        {d.count !== undefined && d.count !== null && (
          <p className="text-muted-foreground">Count: {d.count}</p>
        )}
      </div>
    );
  }
  return null;
}

function MiniChart({ title, data, color, timeRange, isPercentage, isArea }: MiniChartProps) {
  const chartData = data.map((p) => ({
    ...p,
    displayTime: formatTimestamp(p.timestamp, timeRange),
    chartValue: isPercentage ? p.value * 100 : p.value,
  }));

  if (chartData.length === 0) {
    return (
      <div className="bg-card/50 border border-border/30 rounded-lg p-3">
        <h4 className="text-[11px] font-medium text-muted-foreground mb-1">{title}</h4>
        <div className="h-24 flex items-center justify-center text-xs text-muted-foreground/50">
          No data
        </div>
      </div>
    );
  }

  const ChartComponent = isArea ? AreaChart : LineChart;

  return (
    <div className="bg-card/50 border border-border/30 rounded-lg p-3">
      <h4 className="text-[11px] font-medium text-muted-foreground mb-1">{title}</h4>
      <div className="h-24">
        <ResponsiveContainer width="100%" height="100%">
          <ChartComponent data={chartData} margin={{ left: 0, right: 4, top: 4, bottom: 0 }}>
            <XAxis
              dataKey="displayTime"
              tick={{ fontSize: 9, fill: "hsl(var(--muted-foreground))" }}
              tickLine={false}
              axisLine={false}
              interval="preserveStartEnd"
            />
            <YAxis
              tick={{ fontSize: 9, fill: "hsl(var(--muted-foreground))" }}
              tickLine={false}
              axisLine={false}
              width={32}
              tickFormatter={(v) => (isPercentage ? `${v.toFixed(0)}%` : v.toFixed(1))}
              domain={isPercentage ? [0, 100] : ["auto", "auto"]}
            />
            <Tooltip content={<ChartTooltip isPercentage={isPercentage} />} />
            {isArea ? (
              <Area
                type="monotone"
                dataKey="chartValue"
                stroke={color}
                fill={color}
                fillOpacity={0.15}
                strokeWidth={1.5}
                dot={false}
              />
            ) : (
              <Line
                type="monotone"
                dataKey="chartValue"
                stroke={color}
                strokeWidth={1.5}
                dot={{ r: 2, fill: color }}
                activeDot={{ r: 4, fill: color }}
              />
            )}
          </ChartComponent>
        </ResponsiveContainer>
      </div>
    </div>
  );
}

export function TrendsPanel({ workflowName, selectedComponent }: TrendsPanelProps) {
  const [expanded, setExpanded] = useState(true);
  const [timeRange, setTimeRange] = useState<TimeRange>("30d");

  const { trends, loading, error } = useWorkflowTrends(workflowName, timeRange);
  const { trend: componentTrend } = useComponentTrend(workflowName, selectedComponent, timeRange);

  const hasData = trends && trends.snapshot_count > 0;

  return (
    <div className="flex-shrink-0 border border-border/50 rounded-lg bg-card/30">
      {/* Header */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center justify-between px-3 py-2 hover:bg-muted/30 transition-colors rounded-t-lg"
      >
        <div className="flex items-center gap-2">
          {expanded ? (
            <ChevronDown className="w-3.5 h-3.5 text-muted-foreground" />
          ) : (
            <ChevronRight className="w-3.5 h-3.5 text-muted-foreground" />
          )}
          <TrendingUp className="w-4 h-4 text-blue-400" />
          <span className="text-sm font-medium">Trends</span>
          {trends && (
            <span className="text-[10px] text-muted-foreground">
              {trends.snapshot_count} snapshots
            </span>
          )}
        </div>
        {expanded && (
          <div className="flex items-center gap-1" onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()} role="group">
            {TREND_TIME_RANGES.map((r) => (
              <button
                key={r}
                onClick={() => setTimeRange(r)}
                className={`px-2 py-0.5 text-[10px] rounded transition-colors ${
                  timeRange === r
                    ? "bg-blue-600/20 text-blue-400 border border-blue-600/30"
                    : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
                }`}
              >
                {TIME_RANGE_LABELS[r]}
              </button>
            ))}
          </div>
        )}
      </button>

      {/* Content */}
      {expanded && (
        <div className="px-3 pb-3 space-y-2">
          {loading && !trends && (
            <div className="text-xs text-muted-foreground py-4 text-center">Loading trends...</div>
          )}

          {error && <div className="text-xs text-red-400 py-2">{error}</div>}

          {!loading && !hasData && !error && (
            <div className="text-xs text-muted-foreground/60 py-4 text-center">
              No convergence data yet. Run reflection workflows to see trends.
            </div>
          )}

          {hasData && trends && (
            <>
              {/* 2x2 grid of workflow-level charts */}
              <div className="grid grid-cols-2 gap-2">
                <MiniChart
                  title="Convergence Score"
                  data={trends.convergence}
                  color="#3b82f6"
                  timeRange={timeRange}
                  isPercentage
                />
                <MiniChart
                  title="Fix Effectiveness"
                  data={trends.fix_rate}
                  color="#22c55e"
                  timeRange={timeRange}
                  isPercentage
                />
                <MiniChart
                  title="Change Velocity"
                  data={trends.velocity}
                  color="#f97316"
                  timeRange={timeRange}
                />
                <MiniChart
                  title="Total Fixes"
                  data={trends.total_fixes}
                  color="#a855f7"
                  timeRange={timeRange}
                  isArea
                />
              </div>

              {/* Effectiveness trend over time */}
              <EffectivenessTrendWidget workflowName={workflowName} timeRange={timeRange} />

              {/* Component-specific trends when a component is selected */}
              {selectedComponent && componentTrend && componentTrend.health_scores.length > 0 && (
                <div className="pt-1">
                  <p className="text-[10px] text-muted-foreground mb-1.5">
                    Component:{" "}
                    <span className="font-medium text-foreground">
                      {selectedComponent.split("/").pop()}
                    </span>
                  </p>
                  <div className="grid grid-cols-2 gap-2">
                    <MiniChart
                      title="Component Health"
                      data={componentTrend.health_scores}
                      color="#06b6d4"
                      timeRange={timeRange}
                      isPercentage
                    />
                    <MiniChart
                      title="Component Fixes"
                      data={componentTrend.fix_counts}
                      color="#ec4899"
                      timeRange={timeRange}
                    />
                  </div>
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  Bar,
  BarChart,
} from "recharts";
import { useEffectivenessTrend } from "@/hooks/useArchitecture";
import { effectivenessChartData, formatRatePercent, trendEnds } from "@/lib/effectivenessRate";
import type { EffectivenessChartPoint } from "@/lib/effectivenessRate";
import type { TimeRange } from "@/types/performance-metrics";

interface EffectivenessTrendWidgetProps {
  workflowName: string;
  timeRange: TimeRange;
}

function EffectivenessTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: { payload: EffectivenessChartPoint }[];
}) {
  if (active && payload && payload.length) {
    const d = payload[0].payload;
    return (
      <div className="bg-popover border border-border rounded-lg p-2 shadow-lg text-xs">
        <p className="font-medium">{d.bucket}</p>
        <p className="text-green-400">
          Effective: {d.effective}/{d.total} ({formatRatePercent(d.ratePercent)})
        </p>
        {d.ineffective > 0 && <p className="text-yellow-400">Ineffective: {d.ineffective}</p>}
        {d.regression > 0 && <p className="text-red-400">Regression: {d.regression}</p>}
        {d.inconclusive > 0 && <p className="text-zinc-400">Inconclusive: {d.inconclusive}</p>}
        {d.unjudged > 0 && <p className="text-zinc-400">Unjudged: {d.unjudged}</p>}
      </div>
    );
  }
  return null;
}

export function EffectivenessTrendWidget({
  workflowName,
  timeRange,
}: EffectivenessTrendWidgetProps) {
  const { data, loading } = useEffectivenessTrend(workflowName, timeRange);

  if (loading && !data) {
    return null;
  }

  if (!data || data.buckets.length === 0) {
    return null;
  }

  const chartData = effectivenessChartData(data.buckets);

  // Determine trend direction for color
  const { first: firstRate, last: lastRate } = trendEnds(chartData);
  const trendColor =
    firstRate == null || lastRate == null || lastRate >= firstRate ? "#22c55e" : "#ef4444";

  return (
    <div className="grid grid-cols-2 gap-2">
      {/* Effectiveness rate line chart */}
      <div className="bg-card/50 border border-border/30 rounded-lg p-3">
        <h4 className="text-[11px] font-medium text-muted-foreground mb-1">Effectiveness Rate</h4>
        <div className="h-24">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={chartData} margin={{ left: 0, right: 4, top: 4, bottom: 0 }}>
              <XAxis
                dataKey="bucket"
                tick={{
                  fontSize: 9,
                  fill: "hsl(var(--muted-foreground))",
                }}
                tickLine={false}
                axisLine={false}
                interval="preserveStartEnd"
              />
              <YAxis
                tick={{
                  fontSize: 9,
                  fill: "hsl(var(--muted-foreground))",
                }}
                tickLine={false}
                axisLine={false}
                width={32}
                domain={[0, 100]}
                tickFormatter={(v) => `${v}%`}
              />
              <Tooltip content={<EffectivenessTooltip />} />
              <Line
                type="monotone"
                dataKey="ratePercent"
                stroke={trendColor}
                strokeWidth={1.5}
                dot={{ r: 2, fill: trendColor }}
                activeDot={{ r: 4, fill: trendColor }}
              />
            </LineChart>
          </ResponsiveContainer>
        </div>
      </div>

      {/* Fix count bar chart */}
      <div className="bg-card/50 border border-border/30 rounded-lg p-3">
        <h4 className="text-[11px] font-medium text-muted-foreground mb-1">Fix Volume</h4>
        <div className="h-24">
          <ResponsiveContainer width="100%" height="100%">
            <BarChart data={chartData} margin={{ left: 0, right: 4, top: 4, bottom: 0 }}>
              <XAxis
                dataKey="bucket"
                tick={{
                  fontSize: 9,
                  fill: "hsl(var(--muted-foreground))",
                }}
                tickLine={false}
                axisLine={false}
                interval="preserveStartEnd"
              />
              <YAxis
                tick={{
                  fontSize: 9,
                  fill: "hsl(var(--muted-foreground))",
                }}
                tickLine={false}
                axisLine={false}
                width={24}
              />
              <Tooltip content={<EffectivenessTooltip />} />
              <Bar dataKey="total" fill="#6366f1" radius={[2, 2, 0, 0]} />
            </BarChart>
          </ResponsiveContainer>
        </div>
      </div>
    </div>
  );
}

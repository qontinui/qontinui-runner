/**
 * SparklinePanel Component
 *
 * Tiny win-loss bar chart showing step outcomes across iterations.
 * Green bars point up for "pass", red bars point down for "fail".
 */

import { BarChart, Bar, Cell, ResponsiveContainer } from "recharts";
import { cn } from "../../../../../lib/utils";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

interface SparklineValue {
  iteration: number;
  outcome: "pass" | "fail";
}

interface SparklineSeries {
  name: string;
  values: SparklineValue[];
}

const PASS_COLOR = "#22c55e";
const FAIL_COLOR = "#ef4444";

export function SparklinePanel({ data, size }: CanvasPanelComponentProps) {
  const series = (data.series as SparklineSeries[]) ?? [];
  const isCompact = size === "compact";

  if (series.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  const chartHeight = isCompact ? 24 : 32;
  const fontSize = isCompact ? "text-[10px]" : "text-xs";

  return (
    <div className="space-y-1">
      {series.map((s, seriesIdx) => {
        // Transform data: pass = +1, fail = -1
        const chartData = s.values.map((v) => ({
          iteration: v.iteration,
          value: v.outcome === "pass" ? 1 : -1,
          outcome: v.outcome,
        }));

        return (
          <div key={seriesIdx} className="flex items-center gap-2">
            {/* Series name */}
            <div
              className={cn("truncate text-muted-foreground flex-shrink-0", fontSize)}
              style={{ width: isCompact ? 80 : 100 }}
              title={s.name}
            >
              {s.name}
            </div>

            {/* Sparkline chart */}
            <div className="flex-1" style={{ height: chartHeight }}>
              <ResponsiveContainer width="100%" height="100%">
                <BarChart data={chartData} margin={{ top: 0, right: 0, bottom: 0, left: 0 }}>
                  <Bar dataKey="value" barSize={8}>
                    {chartData.map((entry, idx) => (
                      <Cell key={idx} fill={entry.outcome === "pass" ? PASS_COLOR : FAIL_COLOR} />
                    ))}
                  </Bar>
                </BarChart>
              </ResponsiveContainer>
            </div>
          </div>
        );
      })}
    </div>
  );
}

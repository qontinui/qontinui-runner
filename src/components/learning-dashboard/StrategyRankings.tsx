/**
 * StrategyRankings.tsx
 *
 * Displays strategy and tool performance rankings with bar charts.
 */

import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer, Cell } from "recharts";
import { Trophy, Wrench } from "lucide-react";

interface StrategyRankingsProps {
  strategies: Array<[string, number]>;
  tools: Array<[string, number]>;
}

const COLORS = {
  high: "#22c55e",
  medium: "#eab308",
  low: "#ef4444",
};

function getBarColor(value: number): string {
  if (value >= 0.7) return COLORS.high;
  if (value >= 0.5) return COLORS.medium;
  return COLORS.low;
}

function StrategyChart({ strategies }: { strategies: Array<[string, number]> }) {
  const data = strategies.map(([name, rate]) => ({
    name: name.length > 15 ? name.substring(0, 15) + "..." : name,
    fullName: name,
    rate: Math.round(rate * 100),
  }));

  if (data.length === 0) {
    return (
      <div className="h-48 flex items-center justify-center text-gray-500">No strategy data</div>
    );
  }

  return (
    <ResponsiveContainer width="100%" height={Math.max(200, data.length * 40)}>
      <BarChart data={data} layout="vertical" margin={{ top: 5, right: 30, left: 5, bottom: 5 }}>
        <XAxis type="number" domain={[0, 100]} tick={{ fill: "#9ca3af", fontSize: 12 }} />
        <YAxis
          type="category"
          dataKey="name"
          tick={{ fill: "#9ca3af", fontSize: 12 }}
          width={100}
        />
        <Tooltip
          contentStyle={{
            backgroundColor: "#1f2937",
            border: "1px solid #374151",
            borderRadius: "6px",
          }}
          labelStyle={{ color: "#fff" }}
          formatter={(value: number) => [`${value}%`, "Success Rate"]}
          labelFormatter={(_, payload) => payload[0]?.payload?.fullName || ""}
        />
        <Bar dataKey="rate" radius={[0, 4, 4, 0]}>
          {data.map((entry, index) => (
            <Cell key={`cell-${index}`} fill={getBarColor(entry.rate / 100)} />
          ))}
        </Bar>
      </BarChart>
    </ResponsiveContainer>
  );
}

function ToolChart({ tools }: { tools: Array<[string, number]> }) {
  const data = tools.map(([name, count]) => ({
    name: name.length > 15 ? name.substring(0, 15) + "..." : name,
    fullName: name,
    count,
  }));

  if (data.length === 0) {
    return <div className="h-48 flex items-center justify-center text-gray-500">No tool data</div>;
  }

  return (
    <ResponsiveContainer width="100%" height={Math.max(200, data.length * 40)}>
      <BarChart data={data} layout="vertical" margin={{ top: 5, right: 30, left: 5, bottom: 5 }}>
        <XAxis type="number" tick={{ fill: "#9ca3af", fontSize: 12 }} />
        <YAxis
          type="category"
          dataKey="name"
          tick={{ fill: "#9ca3af", fontSize: 12 }}
          width={100}
        />
        <Tooltip
          contentStyle={{
            backgroundColor: "#1f2937",
            border: "1px solid #374151",
            borderRadius: "6px",
          }}
          labelStyle={{ color: "#fff" }}
          formatter={(value: number) => [value, "Uses"]}
          labelFormatter={(_, payload) => payload[0]?.payload?.fullName || ""}
        />
        <Bar dataKey="count" fill="#3b82f6" radius={[0, 4, 4, 0]} />
      </BarChart>
    </ResponsiveContainer>
  );
}

export function StrategyRankings({ strategies, tools }: StrategyRankingsProps) {
  return (
    <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
      {/* Strategy Performance */}
      <div className="bg-gray-800 rounded-lg p-4">
        <div className="flex items-center gap-2 mb-4">
          <Trophy className="w-5 h-5 text-yellow-500" />
          <h3 className="font-semibold text-white">Strategy Performance</h3>
        </div>
        <StrategyChart strategies={strategies} />
        {strategies.length > 0 && (
          <div className="mt-3 flex items-center justify-center gap-4 text-xs text-gray-400">
            <span className="flex items-center gap-1">
              <span className="w-3 h-3 bg-green-500 rounded" /> &gt;70%
            </span>
            <span className="flex items-center gap-1">
              <span className="w-3 h-3 bg-yellow-500 rounded" /> 50-70%
            </span>
            <span className="flex items-center gap-1">
              <span className="w-3 h-3 bg-red-500 rounded" /> &lt;50%
            </span>
          </div>
        )}
      </div>

      {/* Tool Usage */}
      <div className="bg-gray-800 rounded-lg p-4">
        <div className="flex items-center gap-2 mb-4">
          <Wrench className="w-5 h-5 text-blue-500" />
          <h3 className="font-semibold text-white">Tool Usage</h3>
        </div>
        <ToolChart tools={tools} />
      </div>
    </div>
  );
}

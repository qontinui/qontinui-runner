/**
 * CostEffectivenessPanel — SVG scatter plot of cost vs success rate per agent/week.
 * Draws Pareto frontier connecting non-dominated points.
 */

import { useState, useEffect, useCallback, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";

interface CostEffectivenessPoint {
  agent_type: string;
  period_label: string;
  avg_cost_usd: number;
  success_rate: number;
  run_count: number;
}

const AGENT_COLORS: Record<string, string> = {
  spec_analyst: "#3b82f6",
  locator: "#06b6d4",
  implementer: "#f59e0b",
  verifier: "#22c55e",
};

const AGENT_LABELS: Record<string, string> = {
  spec_analyst: "Spec Analyst",
  locator: "Locator",
  implementer: "Implementer",
  verifier: "Verifier",
};

const SVG_W = 500;
const SVG_H = 280;
const PAD = { top: 20, right: 20, bottom: 40, left: 60 };

export function CostEffectivenessPanel() {
  const [points, setPoints] = useState<CostEffectivenessPoint[]>([]);
  const [loading, setLoading] = useState(true);
  const [agentFilter, setAgentFilter] = useState<string>("all");
  const [hovered, setHovered] = useState<number | null>(null);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const data = await invoke<CostEffectivenessPoint[]>(
        "get_agent_cost_effectiveness",
        { agentType: agentFilter === "all" ? null : agentFilter, days: 90 },
      );
      setPoints(data);
    } catch {
      // silently fail
    } finally {
      setLoading(false);
    }
  }, [agentFilter]);

  useEffect(() => {
    load();
  }, [load]);

  const filtered = useMemo(() => {
    if (agentFilter === "all") return points;
    return points.filter((p) => p.agent_type === agentFilter);
  }, [points, agentFilter]);

  // Compute Pareto frontier
  const paretoSet = useMemo(() => {
    const indices = new Set<number>();
    for (let i = 0; i < filtered.length; i++) {
      let dominated = false;
      for (let j = 0; j < filtered.length; j++) {
        if (i === j) continue;
        if (
          filtered[j].success_rate >= filtered[i].success_rate &&
          filtered[j].avg_cost_usd <= filtered[i].avg_cost_usd &&
          (filtered[j].success_rate > filtered[i].success_rate ||
            filtered[j].avg_cost_usd < filtered[i].avg_cost_usd)
        ) {
          dominated = true;
          break;
        }
      }
      if (!dominated) indices.add(i);
    }
    return indices;
  }, [filtered]);

  if (loading) {
    return <div className="text-sm text-zinc-500 p-4">Loading cost-effectiveness data...</div>;
  }

  if (filtered.length === 0) {
    return null;
  }

  // Scale computation
  const costs = filtered.map((p) => p.avg_cost_usd);
  const rates = filtered.map((p) => p.success_rate);
  const minCost = Math.min(...costs) * 0.9;
  const maxCost = Math.max(...costs) * 1.1 || 0.01;
  const minRate = Math.max(0, Math.min(...rates) - 5);
  const maxRate = Math.min(100, Math.max(...rates) + 5);

  const plotW = SVG_W - PAD.left - PAD.right;
  const plotH = SVG_H - PAD.top - PAD.bottom;

  const scaleX = (v: number) => PAD.left + ((v - minCost) / (maxCost - minCost || 1)) * plotW;
  const scaleY = (v: number) => PAD.top + plotH - ((v - minRate) / (maxRate - minRate || 1)) * plotH;

  // Pareto frontier line
  const paretoPoints = [...paretoSet]
    .map((i) => filtered[i])
    .sort((a, b) => a.avg_cost_usd - b.avg_cost_usd);
  const paretoPath =
    paretoPoints.length > 1
      ? paretoPoints
          .map((p, i) => `${i === 0 ? "M" : "L"} ${scaleX(p.avg_cost_usd)} ${scaleY(p.success_rate)}`)
          .join(" ")
      : "";

  const agentTypes = [...new Set(points.map((p) => p.agent_type))];

  return (
    <div>
      <div className="flex items-center justify-between mb-2">
        <div className="text-sm text-zinc-400">Cost-Effectiveness (Pareto)</div>
        <select
          className="bg-zinc-800 text-zinc-200 text-xs px-2 py-1 rounded border border-zinc-700"
          value={agentFilter}
          onChange={(e) => setAgentFilter(e.target.value)}
        >
          <option value="all">All Agents</option>
          {agentTypes.map((t) => (
            <option key={t} value={t}>
              {AGENT_LABELS[t] ?? t}
            </option>
          ))}
        </select>
      </div>

      <div className="bg-zinc-900 border border-zinc-800 rounded-lg p-3">
        <svg width={SVG_W} height={SVG_H} className="w-full" viewBox={`0 0 ${SVG_W} ${SVG_H}`}>
          {/* Grid lines */}
          {[0, 25, 50, 75, 100]
            .filter((v) => v >= minRate && v <= maxRate)
            .map((v) => (
              <g key={`h-${v}`}>
                <line
                  x1={PAD.left}
                  y1={scaleY(v)}
                  x2={SVG_W - PAD.right}
                  y2={scaleY(v)}
                  stroke="#3f3f46"
                  strokeWidth={0.5}
                />
                <text x={PAD.left - 5} y={scaleY(v) + 3} textAnchor="end" fill="#71717a" fontSize={9}>
                  {v}%
                </text>
              </g>
            ))}

          {/* X axis label */}
          <text
            x={PAD.left + plotW / 2}
            y={SVG_H - 5}
            textAnchor="middle"
            fill="#71717a"
            fontSize={10}
          >
            Avg Cost (USD)
          </text>
          {/* Y axis label */}
          <text
            x={12}
            y={PAD.top + plotH / 2}
            textAnchor="middle"
            fill="#71717a"
            fontSize={10}
            transform={`rotate(-90, 12, ${PAD.top + plotH / 2})`}
          >
            Success Rate
          </text>

          {/* Pareto frontier */}
          {paretoPath && (
            <path d={paretoPath} fill="none" stroke="#22d3ee" strokeWidth={1.5} strokeDasharray="4 2" />
          )}

          {/* Data points */}
          {filtered.map((p, i) => {
            const isPareto = paretoSet.has(i);
            const color = AGENT_COLORS[p.agent_type] ?? "#a78bfa";
            const cx = scaleX(p.avg_cost_usd);
            const cy = scaleY(p.success_rate);

            return (
              <g key={i}>
                <circle
                  cx={cx}
                  cy={cy}
                  r={isPareto ? 5 : 3.5}
                  fill={isPareto ? "#22d3ee" : color}
                  stroke={isPareto ? "#06b6d4" : color}
                  strokeWidth={1}
                  opacity={0.85}
                  onMouseEnter={() => setHovered(i)}
                  onMouseLeave={() => setHovered(null)}
                  className="cursor-pointer"
                />
                {hovered === i && (
                  <g>
                    <rect
                      x={cx + 8}
                      y={cy - 30}
                      width={150}
                      height={40}
                      rx={4}
                      fill="#18181b"
                      stroke="#3f3f46"
                    />
                    <text x={cx + 14} y={cy - 15} fill="#d4d4d8" fontSize={9}>
                      {AGENT_LABELS[p.agent_type] ?? p.agent_type} | {p.period_label}
                    </text>
                    <text x={cx + 14} y={cy - 3} fill="#a1a1aa" fontSize={9}>
                      ${p.avg_cost_usd.toFixed(4)} | {p.success_rate.toFixed(1)}% | {p.run_count} runs
                    </text>
                  </g>
                )}
              </g>
            );
          })}
        </svg>
      </div>
    </div>
  );
}

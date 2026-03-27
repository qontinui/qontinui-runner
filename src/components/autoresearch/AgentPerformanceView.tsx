/**
 * AgentPerformanceView — per-agent metrics, comparison table, scatter plot, and recommendation.
 *
 * Polls get_autoresearch_results, extracts per-agent data from pipeline traces,
 * groups by config variant, and computes aggregates.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentType } from "./types";
import type { PipelineAgentTrace, MultiAgentPipelineResult } from "@/types";

interface ExperimentConfig {
  workflow_architecture?: string;
  model?: string;
  multi_agent_pipeline_config?: Record<string, unknown>;
  extra: Record<string, unknown>;
}

interface AggregateMetrics {
  pass_rate: number;
  mean_iterations: number;
  mean_duration_ms: number;
  trial_count: number;
}

interface ExperimentResult {
  config: ExperimentConfig;
  trials: Array<{ task_run_id: string; passed: boolean; duration_ms: number }>;
  aggregate: AggregateMetrics;
  accepted: boolean;
  p_value?: number;
}

interface VariantAggregate {
  key: string;
  passRate: number;
  meanDuration: number;
  meanCost: number;
  meanTokens: number;
  meanQuality: number | null;
  experimentCount: number;
  traceCount: number;
}

function extractAgentTraces(result: ExperimentResult, agentType: AgentType): PipelineAgentTrace[] {
  const extra = result.config.extra ?? {};
  const pipelineResult = extra["pipeline_result"] as MultiAgentPipelineResult | undefined;
  if (!pipelineResult?.agent_traces) return [];
  return pipelineResult.agent_traces.filter((t) => t.agent_type === agentType);
}

function variantKey(result: ExperimentResult, agentType: AgentType): string {
  const extra = result.config.extra ?? {};
  const parts: string[] = [];

  // Check pipeline_agent.* keys in extra
  for (const [k, v] of Object.entries(extra)) {
    if (k.startsWith(`pipeline_agent.${agentType}.`)) {
      const field = k.split(".").pop();
      parts.push(`${field}=${v}`);
    }
  }

  // Check multi_agent_pipeline_config
  const mapConfig = result.config.multi_agent_pipeline_config;
  if (mapConfig) {
    const agentConfig = mapConfig[agentType] as Record<string, unknown> | undefined;
    if (agentConfig) {
      if (agentConfig.model) parts.push(`model=${agentConfig.model}`);
      if (agentConfig.temperature != null) parts.push(`temp=${agentConfig.temperature}`);
      if (agentConfig.max_tokens != null) parts.push(`tokens=${agentConfig.max_tokens}`);
      if (agentConfig.prompt_variant) parts.push(`prompt=${agentConfig.prompt_variant}`);
    }
  }

  return parts.length > 0 ? parts.join(", ") : "(baseline)";
}

function computeVariants(
  results: [number, ExperimentResult][],
  agentType: AgentType,
): VariantAggregate[] {
  const groups = new Map<
    string,
    {
      passRates: number[];
      durations: number[];
      costs: number[];
      tokens: number[];
      qualities: number[];
      expCount: number;
      traceCount: number;
    }
  >();

  for (const [, result] of results) {
    const key = variantKey(result, agentType);
    if (!groups.has(key)) {
      groups.set(key, {
        passRates: [],
        durations: [],
        costs: [],
        tokens: [],
        qualities: [],
        expCount: 0,
        traceCount: 0,
      });
    }
    const g = groups.get(key)!;
    g.passRates.push(result.aggregate.pass_rate);
    g.expCount++;

    const traces = extractAgentTraces(result, agentType);
    for (const trace of traces) {
      g.durations.push(trace.duration_ms);
      g.costs.push(trace.cost_usd);
      g.tokens.push(trace.tokens_in + trace.tokens_out);
      if (trace.output_quality_score != null) {
        g.qualities.push(trace.output_quality_score);
      }
      g.traceCount++;
    }
  }

  const avg = (arr: number[]) => (arr.length > 0 ? arr.reduce((a, b) => a + b, 0) / arr.length : 0);

  return Array.from(groups.entries())
    .map(([key, g]) => ({
      key,
      passRate: avg(g.passRates),
      meanDuration: avg(g.durations),
      meanCost: avg(g.costs),
      meanTokens: avg(g.tokens),
      meanQuality: g.qualities.length > 0 ? avg(g.qualities) : null,
      experimentCount: g.expCount,
      traceCount: g.traceCount,
    }))
    .sort((a, b) => b.passRate - a.passRate);
}

function paretoFront(variants: VariantAggregate[]): Set<string> {
  const front = new Set<string>();
  for (const v of variants) {
    let dominated = false;
    for (const other of variants) {
      if (
        other.key !== v.key &&
        other.passRate >= v.passRate &&
        other.meanCost <= v.meanCost &&
        (other.passRate > v.passRate || other.meanCost < v.meanCost)
      ) {
        dominated = true;
        break;
      }
    }
    if (!dominated) front.add(v.key);
  }
  return front;
}

// ─── Scatter Plot (SVG) ──────────────────────────────────────────────────────

function ScatterPlot({ variants, pareto }: { variants: VariantAggregate[]; pareto: Set<string> }) {
  if (variants.length === 0) return null;

  const W = 500;
  const H = 280;
  const PAD = { top: 20, right: 20, bottom: 40, left: 60 };
  const plotW = W - PAD.left - PAD.right;
  const plotH = H - PAD.top - PAD.bottom;

  const costs = variants.map((v) => v.meanCost);
  const rates = variants.map((v) => v.passRate);
  const minCost = Math.min(...costs) * 0.9;
  const maxCost = Math.max(...costs) * 1.1 || 0.01;
  const minRate = Math.max(0, Math.min(...rates) - 0.05);
  const maxRate = Math.min(1, Math.max(...rates) + 0.05);

  const x = (cost: number) => PAD.left + ((cost - minCost) / (maxCost - minCost || 1)) * plotW;
  const y = (rate: number) =>
    PAD.top + plotH - ((rate - minRate) / (maxRate - minRate || 1)) * plotH;

  // Pareto line: sorted by cost ascending
  const paretoPoints = variants
    .filter((v) => pareto.has(v.key))
    .sort((a, b) => a.meanCost - b.meanCost);
  const paretoPath =
    paretoPoints.length > 1
      ? paretoPoints
          .map((v, i) => `${i === 0 ? "M" : "L"}${x(v.meanCost)},${y(v.passRate)}`)
          .join(" ")
      : "";

  return (
    <svg width={W} height={H} className="bg-zinc-900/50 rounded-lg border border-zinc-700">
      {/* Axes */}
      <line
        x1={PAD.left}
        y1={PAD.top + plotH}
        x2={PAD.left + plotW}
        y2={PAD.top + plotH}
        stroke="#52525b"
      />
      <line x1={PAD.left} y1={PAD.top} x2={PAD.left} y2={PAD.top + plotH} stroke="#52525b" />

      {/* Axis labels */}
      <text
        x={PAD.left + plotW / 2}
        y={H - 5}
        textAnchor="middle"
        className="fill-zinc-500 text-[10px]"
      >
        Cost (USD per agent call)
      </text>
      <text
        x={12}
        y={PAD.top + plotH / 2}
        textAnchor="middle"
        transform={`rotate(-90, 12, ${PAD.top + plotH / 2})`}
        className="fill-zinc-500 text-[10px]"
      >
        Pass Rate
      </text>

      {/* Pareto front line */}
      {paretoPath && (
        <path d={paretoPath} fill="none" stroke="#22d3ee" strokeWidth={1.5} strokeDasharray="4 2" />
      )}

      {/* Data points */}
      {variants.map((v) => (
        <g key={v.key}>
          <circle
            cx={x(v.meanCost)}
            cy={y(v.passRate)}
            r={pareto.has(v.key) ? 6 : 4}
            fill={pareto.has(v.key) ? "#22d3ee" : "#a78bfa"}
            fillOpacity={0.8}
            stroke={pareto.has(v.key) ? "#06b6d4" : "#7c3aed"}
            strokeWidth={1.5}
          />
          <text x={x(v.meanCost) + 8} y={y(v.passRate) + 3} className="fill-zinc-400 text-[9px]">
            {v.key.length > 20 ? v.key.slice(0, 20) + "..." : v.key}
          </text>
        </g>
      ))}

      {/* Tick marks */}
      {[0, 0.25, 0.5, 0.75, 1].map((frac) => {
        const cost = minCost + frac * (maxCost - minCost);
        return (
          <text
            key={`x-${frac}`}
            x={x(cost)}
            y={PAD.top + plotH + 14}
            textAnchor="middle"
            className="fill-zinc-600 text-[9px]"
          >
            ${cost.toFixed(3)}
          </text>
        );
      })}
      {[0, 0.25, 0.5, 0.75, 1].map((frac) => {
        const rate = minRate + frac * (maxRate - minRate);
        return (
          <text
            key={`y-${frac}`}
            x={PAD.left - 5}
            y={y(rate) + 3}
            textAnchor="end"
            className="fill-zinc-600 text-[9px]"
          >
            {(rate * 100).toFixed(0)}%
          </text>
        );
      })}
    </svg>
  );
}

// ─── Main Component ─────────────────────────────────────────────────────────

interface Props {
  selectedAgent: AgentType;
  live?: boolean;
  historicalResults?: [number, ExperimentResult][];
}

export function AgentPerformanceView({ selectedAgent, live, historicalResults }: Props) {
  const [results, setResults] = useState<[number, ExperimentResult][]>([]);

  const fetchResults = useCallback(async () => {
    if (historicalResults) return;
    try {
      const r = await invoke<[number, ExperimentResult][]>("get_autoresearch_results");
      setResults(r);
    } catch {
      // Campaign may not exist yet
    }
  }, [historicalResults]);

  useEffect(() => {
    if (historicalResults) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- sync prop to local state
      setResults(historicalResults);
      return;
    }
    fetchResults();
    if (live) {
      const id = setInterval(fetchResults, 5000);
      return () => clearInterval(id);
    }
  }, [fetchResults, live, historicalResults]);

  const variants = computeVariants(results, selectedAgent);
  const pareto = paretoFront(variants);

  if (variants.length === 0) {
    return (
      <div className="text-sm text-zinc-500 p-4">
        No per-agent results yet. Start a per-agent campaign in the Configure view.
      </div>
    );
  }

  const best = variants[0]; // sorted by passRate desc
  const totalConfigs = variants.length;
  const bestPassRate = best.passRate;
  const avgCost = variants.reduce((s, v) => s + v.meanCost, 0) / variants.length;
  const avgDuration = variants.reduce((s, v) => s + v.meanDuration, 0) / variants.length;

  // Find best values for cell highlighting
  const bestValues = {
    passRate: Math.max(...variants.map((v) => v.passRate)),
    duration: Math.min(...variants.map((v) => v.meanDuration)),
    cost: Math.min(...variants.map((v) => v.meanCost)),
    tokens: Math.min(...variants.map((v) => v.meanTokens)),
    quality: Math.max(...variants.filter((v) => v.meanQuality != null).map((v) => v.meanQuality!)),
  };

  function cellHighlight(value: number, best: number, lowerIsBetter = false): string {
    const isBest = lowerIsBetter ? value <= best : value >= best;
    return isBest ? "text-green-400 font-bold" : "text-zinc-300";
  }

  // Pareto recommendation
  const paretoVariants = variants.filter((v) => pareto.has(v.key));
  const cheapestPareto = paretoVariants.sort((a, b) => a.meanCost - b.meanCost)[0];

  return (
    <div className="p-4 space-y-4">
      {/* Summary Cards */}
      <div className="grid grid-cols-5 gap-2">
        {[
          { label: "Configs Tested", value: totalConfigs },
          { label: "Best Pass Rate", value: `${(bestPassRate * 100).toFixed(1)}%` },
          { label: "Best Config", value: best.key },
          { label: "Mean Agent Cost", value: `$${avgCost.toFixed(4)}` },
          { label: "Mean Agent Duration", value: `${(avgDuration / 1000).toFixed(1)}s` },
        ].map((card) => (
          <div
            key={card.label}
            className="bg-zinc-800/50 border border-zinc-700 rounded-md px-3 py-2"
          >
            <div className="text-[10px] text-zinc-500">{card.label}</div>
            <div className="text-sm font-bold text-zinc-200 truncate" title={String(card.value)}>
              {card.value}
            </div>
          </div>
        ))}
      </div>

      {/* Comparison Table */}
      <div className="border border-zinc-700 rounded-lg overflow-hidden">
        <table className="w-full text-xs">
          <thead className="bg-zinc-800">
            <tr>
              <th className="text-left px-3 py-2 text-zinc-400">Config Variant</th>
              <th className="text-center px-3 py-2 text-zinc-400">Pass %</th>
              <th className="text-center px-3 py-2 text-zinc-400">Dur</th>
              <th className="text-center px-3 py-2 text-zinc-400">Cost</th>
              <th className="text-center px-3 py-2 text-zinc-400">Tokens</th>
              <th className="text-center px-3 py-2 text-zinc-400">Quality</th>
              <th className="text-center px-3 py-2 text-zinc-400">Exp</th>
            </tr>
          </thead>
          <tbody>
            {variants.map((v) => (
              <tr
                key={v.key}
                className={`border-t border-zinc-800 ${pareto.has(v.key) ? "bg-cyan-900/10" : ""}`}
              >
                <td className="px-3 py-1.5 font-mono text-zinc-200">
                  {v.key}
                  {pareto.has(v.key) && (
                    <span className="ml-1 text-[9px] text-cyan-400">pareto</span>
                  )}
                </td>
                <td
                  className={`px-3 py-1.5 text-center ${cellHighlight(v.passRate, bestValues.passRate)}`}
                >
                  {(v.passRate * 100).toFixed(1)}%
                </td>
                <td
                  className={`px-3 py-1.5 text-center ${cellHighlight(v.meanDuration, bestValues.duration, true)}`}
                >
                  {(v.meanDuration / 1000).toFixed(1)}s
                </td>
                <td
                  className={`px-3 py-1.5 text-center ${cellHighlight(v.meanCost, bestValues.cost, true)}`}
                >
                  ${v.meanCost.toFixed(4)}
                </td>
                <td
                  className={`px-3 py-1.5 text-center ${cellHighlight(v.meanTokens, bestValues.tokens, true)}`}
                >
                  {(v.meanTokens / 1000).toFixed(1)}k
                </td>
                <td className="px-3 py-1.5 text-center text-zinc-400">
                  {v.meanQuality != null ? v.meanQuality.toFixed(2) : "N/A"}
                </td>
                <td className="px-3 py-1.5 text-center text-zinc-500">{v.experimentCount}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {/* Scatter Plot */}
      <div className="border border-zinc-700 rounded-lg p-3">
        <div className="text-xs font-medium text-zinc-300 mb-2">Cost vs Pass Rate</div>
        <ScatterPlot variants={variants} pareto={pareto} />
      </div>

      {/* Recommendation */}
      <div className="border border-zinc-700 rounded-lg p-4 bg-zinc-800/30">
        <div className="text-sm font-bold text-zinc-200 mb-1">Recommendation</div>
        <div className="text-sm text-zinc-300">
          <span className="text-green-400 font-bold">{best.key}</span> achieves highest pass rate at{" "}
          <span className="font-mono">{(best.passRate * 100).toFixed(1)}%</span> / $
          {best.meanCost.toFixed(4)} per call.
          {cheapestPareto && cheapestPareto.key !== best.key && (
            <>
              {" "}
              <span className="text-cyan-400 font-bold">{cheapestPareto.key}</span> is
              Pareto-optimal at{" "}
              <span className="font-mono">{(cheapestPareto.passRate * 100).toFixed(1)}%</span> pass
              / ${cheapestPareto.meanCost.toFixed(4)}.
            </>
          )}
        </div>
      </div>
    </div>
  );
}

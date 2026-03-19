/**
 * Autoresearch Dashboard — experiment history, current control, convergence graph.
 *
 * Polls the backend for campaign status and experiment results,
 * renders a summary header, convergence sparkline, and results table.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { PipelineTraceView } from "./PipelineTraceView";
import type { MultiAgentPipelineResult } from "@/types";

// ─── Types (mirror Rust types) ───────────────────────────────────────────────

interface ExperimentConfig {
  model?: string;
  max_iterations?: number;
  multi_agent_mode?: boolean;
  max_context_tokens?: number;
  workflow_architecture?: string;
  multi_agent_pipeline_config?: Record<string, unknown>;
  extra: Record<string, unknown>;
}

interface AggregateMetrics {
  pass_rate: number;
  mean_iterations: number;
  mean_duration_ms: number;
  trial_count: number;
}

interface TrialResult {
  task_run_id: string;
  passed: boolean;
  iterations_used: number;
  duration_ms: number;
  workflow_id?: string;
}

interface ExperimentResult {
  config: ExperimentConfig;
  trials: TrialResult[];
  aggregate: AggregateMetrics;
  accepted: boolean;
  reason: string;
  p_value?: number;
  ai_recommendation?: string;
}

interface CampaignStatus {
  campaign_id: string;
  name: string;
  status: "running" | "stopped" | "completed" | "failed" | "converged";
  experiment_count: number;
  accepted_count: number;
  current_control: ExperimentConfig;
  current_experiment: ExperimentConfig | null;
  started_at: string | null;
  error: string | null;
  experiments_since_last_accept: number;
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

function configSummary(config: ExperimentConfig): string {
  const parts: string[] = [];
  if (config.workflow_architecture) parts.push(`arch=${config.workflow_architecture}`);
  if (config.model) parts.push(`model=${config.model}`);
  if (config.max_iterations != null) parts.push(`iter=${config.max_iterations}`);
  if (config.multi_agent_mode != null) parts.push(`multi=${config.multi_agent_mode}`);
  if (config.max_context_tokens != null) parts.push(`ctx=${config.max_context_tokens}`);
  return parts.length > 0 ? parts.join(", ") : "(default)";
}

function statusColor(status: CampaignStatus["status"]): string {
  switch (status) {
    case "running": return "text-blue-400";
    case "completed": return "text-green-400";
    case "converged": return "text-yellow-400";
    case "stopped": return "text-gray-400";
    case "failed": return "text-red-400";
  }
}

// ─── Convergence Sparkline ───────────────────────────────────────────────────

function ConvergenceSparkline({ results }: { results: [number, ExperimentResult][] }) {
  if (results.length === 0) return null;

  const width = 400;
  const height = 80;
  const padding = 4;
  const usable = width - 2 * padding;
  const maxExp = results.length;

  const points = results.map(([num, r], i) => {
    const x = padding + (i / Math.max(maxExp - 1, 1)) * usable;
    const y = height - padding - r.aggregate.pass_rate * (height - 2 * padding);
    return { x, y, accepted: r.accepted, num };
  });

  const pathD = points.map((p, i) => `${i === 0 ? "M" : "L"} ${p.x} ${p.y}`).join(" ");

  return (
    <svg width={width} height={height} className="border border-zinc-700 rounded bg-zinc-900/50">
      {/* Grid line at 50% */}
      <line x1={padding} y1={height / 2} x2={width - padding} y2={height / 2}
        stroke="#444" strokeDasharray="2,2" />
      {/* Line */}
      <path d={pathD} fill="none" stroke="#60a5fa" strokeWidth={1.5} />
      {/* Points */}
      {points.map((p) => (
        <circle key={p.num} cx={p.x} cy={p.y} r={3}
          fill={p.accepted ? "#4ade80" : "#f87171"} />
      ))}
      {/* Y-axis labels */}
      <text x={2} y={12} fill="#888" fontSize={9}>100%</text>
      <text x={2} y={height - 2} fill="#888" fontSize={9}>0%</text>
    </svg>
  );
}

// ─── Main Dashboard ──────────────────────────────────────────────────────────

export function AutoresearchDashboard() {
  const [status, setStatus] = useState<CampaignStatus | null>(null);
  const [results, setResults] = useState<[number, ExperimentResult][]>([]);
  const [selected, setSelected] = useState<number | null>(null);

  const fetchData = useCallback(async () => {
    try {
      const [s, r] = await Promise.all([
        invoke<CampaignStatus>("get_autoresearch_status"),
        invoke<[number, ExperimentResult][]>("get_autoresearch_results"),
      ]);
      setStatus(s);
      setResults(r);
    } catch {
      // Campaign may not exist yet
    }
  }, []);

  useEffect(() => {
    fetchData();
    const id = setInterval(fetchData, 3000);
    return () => clearInterval(id);
  }, [fetchData]);

  if (!status || !status.campaign_id) {
    return (
      <div className="text-sm text-zinc-500 p-4">
        No autoresearch campaign active.
      </div>
    );
  }

  const acceptRate = status.experiment_count > 0
    ? ((status.accepted_count / status.experiment_count) * 100).toFixed(1)
    : "0.0";

  const selectedResult = selected != null
    ? results.find(([n]) => n === selected)?.[1]
    : null;

  return (
    <div className="flex flex-col gap-3 p-3 text-sm">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <span className="font-bold">{status.name}</span>
          <span className={`ml-2 ${statusColor(status.status)}`}>
            {status.status}
          </span>
        </div>
        <div className="flex gap-4 text-xs text-zinc-400">
          <span>Experiments: {status.experiment_count}</span>
          <span>Accepted: {status.accepted_count} ({acceptRate}%)</span>
          <span>Since last accept: {status.experiments_since_last_accept}</span>
        </div>
      </div>

      {/* Current control */}
      <div className="text-xs bg-zinc-800/50 rounded px-2 py-1">
        <span className="text-zinc-500">Control: </span>
        <span className="font-mono">{configSummary(status.current_control)}</span>
      </div>

      {/* Error/convergence message */}
      {status.error && (
        <div className="text-xs bg-yellow-900/30 text-yellow-400 rounded px-2 py-1">
          {status.error}
        </div>
      )}

      {/* Convergence sparkline */}
      <ConvergenceSparkline results={results} />

      {/* Results table */}
      <div className="border border-zinc-700 rounded overflow-auto max-h-80">
        <table className="w-full text-xs">
          <thead className="bg-zinc-800 sticky top-0">
            <tr>
              <th className="text-left px-2 py-1">#</th>
              <th className="text-left px-2 py-1">Architecture</th>
              <th className="text-left px-2 py-1">Config</th>
              <th className="text-center px-2 py-1">Pass Rate</th>
              <th className="text-center px-2 py-1">Iter</th>
              <th className="text-center px-2 py-1">Duration</th>
              <th className="text-center px-2 py-1">p-value</th>
              <th className="text-center px-2 py-1">Result</th>
            </tr>
          </thead>
          <tbody>
            {results.map(([num, r]) => (
              <tr
                key={num}
                className={`border-t border-zinc-800 cursor-pointer hover:bg-zinc-800/50 ${
                  selected === num ? "bg-zinc-700/50" : ""
                }`}
                onClick={() => setSelected(num)}
              >
                <td className="px-2 py-1 font-mono">{num}</td>
                <td className="px-2 py-1 text-zinc-400">
                  {r.config.workflow_architecture || "traditional"}
                </td>
                <td className="px-2 py-1 font-mono truncate max-w-48" title={configSummary(r.config)}>
                  {configSummary(r.config)}
                </td>
                <td className="px-2 py-1 text-center">
                  {(r.aggregate.pass_rate * 100).toFixed(0)}%
                </td>
                <td className="px-2 py-1 text-center">
                  {r.aggregate.mean_iterations.toFixed(1)}
                </td>
                <td className="px-2 py-1 text-center">
                  {(r.aggregate.mean_duration_ms / 1000).toFixed(1)}s
                </td>
                <td className="px-2 py-1 text-center text-zinc-500">
                  {r.p_value != null ? r.p_value.toFixed(3) : "-"}
                </td>
                <td className={`px-2 py-1 text-center font-bold ${
                  r.accepted ? "text-green-400" : "text-red-400"
                }`}>
                  {r.accepted ? "ACCEPT" : "reject"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {/* Detail panel */}
      {selectedResult && (
        <div className="border border-zinc-700 rounded p-2 bg-zinc-800/30">
          <div className="font-bold mb-1">Experiment #{selected} Details</div>
          <div className="text-xs text-zinc-400 mb-2">{selectedResult.reason}</div>
          {selectedResult.ai_recommendation && (
            <div className="text-xs bg-blue-900/20 text-blue-300 rounded p-2 mb-2">
              <div className="font-bold mb-1">AI Recommendation</div>
              <pre className="whitespace-pre-wrap">{selectedResult.ai_recommendation}</pre>
            </div>
          )}
          {selectedResult.config.workflow_architecture === "multi_agent_pipeline" && (
            <PipelineTraceView
              result={
                (selectedResult.config.extra?.pipeline_result as MultiAgentPipelineResult) ?? null
              }
            />
          )}
          <div className="grid grid-cols-2 gap-2 text-xs">
            <div>
              <span className="text-zinc-500">Config:</span>
              <pre className="bg-zinc-900 rounded p-1 mt-1 overflow-auto max-h-32">
                {JSON.stringify(selectedResult.config, null, 2)}
              </pre>
            </div>
            <div>
              <span className="text-zinc-500">Trials ({selectedResult.trials.length}):</span>
              <div className="mt-1 space-y-1">
                {selectedResult.trials.map((t, i) => (
                  <div key={i} className="flex gap-2">
                    <span className={t.passed ? "text-green-400" : "text-red-400"}>
                      {t.passed ? "PASS" : "FAIL"}
                    </span>
                    <span className="text-zinc-500">
                      {t.iterations_used} iter, {(t.duration_ms / 1000).toFixed(1)}s
                    </span>
                    {t.workflow_id && (
                      <span className="text-zinc-600">[{t.workflow_id}]</span>
                    )}
                  </div>
                ))}
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

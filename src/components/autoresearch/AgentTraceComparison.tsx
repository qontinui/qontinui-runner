/**
 * AgentTraceComparison — side-by-side drill-down comparing two config variants.
 *
 * User selects Config A and Config B from dropdowns, sees metrics and
 * input/output snapshots side-by-side.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentType } from "./types";
import type { PipelineAgentTrace, MultiAgentPipelineResult } from "@/types";

interface ExperimentConfig {
  extra: Record<string, unknown>;
  multi_agent_pipeline_config?: Record<string, unknown>;
}

interface ExperimentResult {
  config: ExperimentConfig;
  aggregate: { pass_rate: number };
  trials: Array<{ passed: boolean }>;
}

interface TraceWithContext {
  trace: PipelineAgentTrace;
  goalAchieved: boolean;
  variantKey: string;
}

function extractTraces(
  result: ExperimentResult,
  agentType: AgentType,
): { traces: PipelineAgentTrace[]; goalAchieved: boolean } {
  const extra = result.config.extra ?? {};
  const pipelineResult = extra["pipeline_result"] as MultiAgentPipelineResult | undefined;
  if (!pipelineResult?.agent_traces) return { traces: [], goalAchieved: false };
  return {
    traces: pipelineResult.agent_traces.filter((t) => t.agent_type === agentType),
    goalAchieved: pipelineResult.goal_achieved,
  };
}

function buildVariantKey(result: ExperimentResult, agentType: AgentType): string {
  const extra = result.config.extra ?? {};
  const parts: string[] = [];
  for (const [k, v] of Object.entries(extra)) {
    if (k.startsWith(`pipeline_agent.${agentType}.`)) {
      const field = k.split(".").pop();
      parts.push(`${field}=${v}`);
    }
  }
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

function TracePanel({ item }: { item: TraceWithContext | null }) {
  const [expanded, setExpanded] = useState<"input" | "output" | null>(null);

  if (!item) {
    return (
      <div className="flex-1 bg-zinc-900/30 border border-zinc-700 rounded-lg p-4 text-xs text-zinc-500">
        Select a config variant above
      </div>
    );
  }

  const { trace, goalAchieved } = item;

  return (
    <div className="flex-1 bg-zinc-900/30 border border-zinc-700 rounded-lg p-3 space-y-2">
      {/* Header */}
      <div className="text-xs font-medium text-zinc-200">{item.variantKey}</div>

      {/* Metrics */}
      <div className="grid grid-cols-2 gap-1 text-[11px]">
        <div className="text-zinc-500">Duration:</div>
        <div className="text-zinc-200">{(trace.duration_ms / 1000).toFixed(1)}s</div>
        <div className="text-zinc-500">Tokens:</div>
        <div className="text-zinc-200">
          {trace.tokens_in.toLocaleString()} in / {trace.tokens_out.toLocaleString()} out
        </div>
        <div className="text-zinc-500">Cost:</div>
        <div className="text-zinc-200">${trace.cost_usd.toFixed(4)}</div>
        <div className="text-zinc-500">Result:</div>
        <div className={goalAchieved ? "text-green-400" : "text-red-400"}>
          {goalAchieved ? "PASS" : "FAIL"}
        </div>
        {trace.downstream_success != null && (
          <>
            <div className="text-zinc-500">Downstream:</div>
            <div className={trace.downstream_success ? "text-green-400" : "text-red-400"}>
              {trace.downstream_success ? "YES" : "NO"}
            </div>
          </>
        )}
        {trace.output_quality_score != null && (
          <>
            <div className="text-zinc-500">Quality:</div>
            <div className="text-zinc-200">{trace.output_quality_score.toFixed(2)}</div>
          </>
        )}
      </div>

      {/* Input Snapshot */}
      <div>
        <button
          onClick={() => setExpanded(expanded === "input" ? null : "input")}
          className="text-[10px] text-zinc-500 hover:text-zinc-300"
        >
          {expanded === "input" ? "- " : "+ "}Input Snapshot
        </button>
        {expanded === "input" && (
          <pre className="bg-zinc-900 rounded p-2 mt-1 overflow-auto max-h-40 text-[10px] text-zinc-300">
            {JSON.stringify(trace.input_snapshot, null, 2)}
          </pre>
        )}
      </div>

      {/* Output Snapshot */}
      <div>
        <button
          onClick={() => setExpanded(expanded === "output" ? null : "output")}
          className="text-[10px] text-zinc-500 hover:text-zinc-300"
        >
          {expanded === "output" ? "- " : "+ "}Output Snapshot
        </button>
        {expanded === "output" && (
          <pre className="bg-zinc-900 rounded p-2 mt-1 overflow-auto max-h-40 text-[10px] text-zinc-300">
            {JSON.stringify(trace.output_snapshot, null, 2)}
          </pre>
        )}
      </div>
    </div>
  );
}

// ─── Main Component ─────────────────────────────────────────────────────────

interface Props {
  selectedAgent: AgentType;
}

export function AgentTraceComparison({ selectedAgent }: Props) {
  const [results, setResults] = useState<[number, ExperimentResult][]>([]);
  const [configA, setConfigA] = useState<string>("");
  const [configB, setConfigB] = useState<string>("");

  const fetchResults = useCallback(async () => {
    try {
      const r = await invoke<[number, ExperimentResult][]>("get_autoresearch_results");
      setResults(r);
    } catch {
      // Campaign may not exist
    }
  }, []);

  useEffect(() => {
    fetchResults();
    const id = setInterval(fetchResults, 5000);
    return () => clearInterval(id);
  }, [fetchResults]);

  // Build variant groups with their traces
  const variantTraces = new Map<string, TraceWithContext[]>();
  for (const [, result] of results) {
    const key = buildVariantKey(result, selectedAgent);
    const { traces, goalAchieved } = extractTraces(result, selectedAgent);
    if (!variantTraces.has(key)) variantTraces.set(key, []);
    for (const trace of traces) {
      variantTraces.get(key)!.push({ trace, goalAchieved, variantKey: key });
    }
  }

  const variantKeys = Array.from(variantTraces.keys());

  // Pick first trace from each selected variant for comparison
  const traceA = configA ? (variantTraces.get(configA)?.[0] ?? null) : null;
  const traceB = configB ? (variantTraces.get(configB)?.[0] ?? null) : null;

  if (variantKeys.length === 0) {
    return (
      <div className="text-sm text-zinc-500 p-4">
        No trace data available for comparison. Run a per-agent campaign first.
      </div>
    );
  }

  return (
    <div className="p-4 space-y-3">
      {/* Config selectors */}
      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className="block text-xs text-zinc-500 mb-1">Config A</label>
          <select
            value={configA}
            onChange={(e) => setConfigA(e.target.value)}
            className="w-full px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded-md text-xs text-zinc-200 [&>option]:text-black [&>option]:bg-white"
            style={{ colorScheme: "dark" }}
          >
            <option value="">Select variant...</option>
            {variantKeys.map((k) => (
              <option key={k} value={k}>
                {k} ({variantTraces.get(k)!.length} traces)
              </option>
            ))}
          </select>
        </div>
        <div>
          <label className="block text-xs text-zinc-500 mb-1">Config B</label>
          <select
            value={configB}
            onChange={(e) => setConfigB(e.target.value)}
            className="w-full px-2 py-1.5 bg-zinc-800 border border-zinc-700 rounded-md text-xs text-zinc-200 [&>option]:text-black [&>option]:bg-white"
            style={{ colorScheme: "dark" }}
          >
            <option value="">Select variant...</option>
            {variantKeys.map((k) => (
              <option key={k} value={k}>
                {k} ({variantTraces.get(k)!.length} traces)
              </option>
            ))}
          </select>
        </div>
      </div>

      {/* Side-by-side panels */}
      <div className="flex gap-3">
        <TracePanel item={traceA} />
        <TracePanel item={traceB} />
      </div>
    </div>
  );
}

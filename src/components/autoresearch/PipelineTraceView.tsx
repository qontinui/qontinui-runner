/**
 * PipelineTraceView — visualize multi-agent pipeline execution results.
 *
 * Shows summary cards, DAG level visualization, and agent trace table.
 */

import { useState } from "react";
import type { MultiAgentPipelineResult, PipelineAgentTrace } from "@/types";

// ─── Summary Cards ──────────────────────────────────────────────────────────

function SummaryCards({ result }: { result: MultiAgentPipelineResult }) {
  const cards = [
    {
      label: "Total Criteria",
      value: result.total_criteria,
      sub: `${result.passed_criteria} passed`,
    },
    { label: "Subtrees", value: result.subtree_results.length },
    { label: "Total Cost", value: `$${result.total_cost_usd.toFixed(4)}` },
    { label: "Total Tokens", value: result.total_tokens.toLocaleString() },
  ];

  return (
    <div className="grid grid-cols-4 gap-2">
      {cards.map((c) => (
        <div key={c.label} className="bg-zinc-800/50 border border-zinc-700 rounded-md px-3 py-2">
          <div className="text-xs text-zinc-500">{c.label}</div>
          <div className="text-lg font-bold text-zinc-200">{c.value}</div>
          {c.sub && <div className="text-xs text-zinc-400">{c.sub}</div>}
        </div>
      ))}
    </div>
  );
}

// ─── DAG Level Visualization ────────────────────────────────────────────────

function DAGView({ result }: { result: MultiAgentPipelineResult }) {
  const { dag } = result;
  if (!dag || !dag.levels || dag.levels.length === 0) {
    return <div className="text-xs text-zinc-500">No DAG data available.</div>;
  }

  // Build a set of passed criteria from subtree results
  const passedCriteria = new Set<string>();
  const failedCriteria = new Set<string>();
  for (const st of result.subtree_results) {
    for (const lr of st.level_results) {
      for (const cr of lr.criterion_results) {
        if (cr.passed) passedCriteria.add(cr.criterion_id);
        else failedCriteria.add(cr.criterion_id);
      }
    }
  }

  return (
    <div className="space-y-1">
      <div className="text-xs text-zinc-500 mb-2">
        DAG Levels ({dag.levels.length} levels, {dag.roots.length} roots)
      </div>
      {dag.levels.map((level, i) => (
        <div key={i} className="flex items-center gap-2">
          <span className="text-xs text-zinc-600 w-8 shrink-0">L{i}</span>
          <div className="flex gap-1 flex-wrap">
            {level.map((criterionId) => {
              const bg = passedCriteria.has(criterionId)
                ? "bg-green-900/50 border-green-700 text-green-300"
                : failedCriteria.has(criterionId)
                  ? "bg-red-900/50 border-red-700 text-red-300"
                  : "bg-zinc-800 border-zinc-700 text-zinc-400";
              return (
                <span
                  key={criterionId}
                  className={`text-xs px-2 py-0.5 border rounded ${bg}`}
                  title={criterionId}
                >
                  {criterionId.length > 20 ? criterionId.slice(0, 20) + "..." : criterionId}
                </span>
              );
            })}
          </div>
        </div>
      ))}
    </div>
  );
}

// ─── Agent Trace Table ──────────────────────────────────────────────────────

function TraceRow({ trace }: { trace: PipelineAgentTrace }) {
  const [expanded, setExpanded] = useState(false);

  return (
    <>
      <tr
        className="border-t border-zinc-800 cursor-pointer hover:bg-zinc-800/50"
        onClick={() => setExpanded(!expanded)}
      >
        <td className="px-2 py-1 font-mono">{trace.agent_type}</td>
        <td className="px-2 py-1 font-mono text-zinc-400">{trace.agent_id}</td>
        <td className="px-2 py-1 text-center">{(trace.duration_ms / 1000).toFixed(1)}s</td>
        <td className="px-2 py-1 text-center">{trace.tokens_in.toLocaleString()}</td>
        <td className="px-2 py-1 text-center">{trace.tokens_out.toLocaleString()}</td>
        <td className="px-2 py-1 text-center">${trace.cost_usd.toFixed(4)}</td>
        <td className="px-2 py-1 text-center">
          {trace.downstream_success != null && (
            <span className={trace.downstream_success ? "text-green-400" : "text-red-400"}>
              {trace.downstream_success ? "YES" : "NO"}
            </span>
          )}
        </td>
      </tr>
      {expanded && (
        <tr className="border-t border-zinc-800/50">
          <td colSpan={7} className="px-2 py-2">
            <div className="grid grid-cols-2 gap-2 text-xs">
              <div>
                <span className="text-zinc-500">Input:</span>
                <pre className="bg-zinc-900 rounded p-1 mt-1 overflow-auto max-h-32 text-zinc-300">
                  {JSON.stringify(trace.input_snapshot, null, 2)}
                </pre>
              </div>
              <div>
                <span className="text-zinc-500">Output:</span>
                <pre className="bg-zinc-900 rounded p-1 mt-1 overflow-auto max-h-32 text-zinc-300">
                  {JSON.stringify(trace.output_snapshot, null, 2)}
                </pre>
              </div>
            </div>
          </td>
        </tr>
      )}
    </>
  );
}

// ─── Main Component ─────────────────────────────────────────────────────────

interface Props {
  result: MultiAgentPipelineResult | null | undefined;
}

export function PipelineTraceView({ result }: Props) {
  if (!result) {
    return (
      <div className="text-xs text-zinc-500 p-2">
        No pipeline trace data available for this experiment.
      </div>
    );
  }

  return (
    <div className="space-y-4 p-2">
      {/* Summary */}
      <SummaryCards result={result} />

      {/* Status */}
      <div className="flex gap-3 text-xs">
        <span className={result.goal_achieved ? "text-green-400" : "text-red-400"}>
          {result.goal_achieved ? "Goal Achieved" : "Goal Not Achieved"}
        </span>
        <span className="text-zinc-500">Iterations: {result.total_iterations}</span>
        {result.was_stopped && <span className="text-yellow-400">Stopped early</span>}
        {result.max_iterations_reached && (
          <span className="text-yellow-400">Max iterations reached</span>
        )}
      </div>

      {/* DAG */}
      <div className="border border-zinc-700 rounded-md p-3">
        <div className="text-sm font-medium text-zinc-300 mb-2">Execution DAG</div>
        <DAGView result={result} />
      </div>

      {/* Agent Traces */}
      <div className="border border-zinc-700 rounded overflow-auto max-h-80">
        <table className="w-full text-xs">
          <thead className="bg-zinc-800 sticky top-0">
            <tr>
              <th className="text-left px-2 py-1">Type</th>
              <th className="text-left px-2 py-1">ID</th>
              <th className="text-center px-2 py-1">Duration</th>
              <th className="text-center px-2 py-1">Tokens In</th>
              <th className="text-center px-2 py-1">Tokens Out</th>
              <th className="text-center px-2 py-1">Cost</th>
              <th className="text-center px-2 py-1">Success</th>
            </tr>
          </thead>
          <tbody>
            {result.agent_traces.map((t, i) => (
              <TraceRow key={i} trace={t} />
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

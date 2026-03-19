/**
 * ResultsTab — architecture comparison grid.
 *
 * Groups autoresearch experiments by workflow_architecture and
 * displays aggregated metrics in a comparison table.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface ExperimentConfig {
  workflow_architecture?: string;
  model?: string;
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
  aggregate: AggregateMetrics;
  accepted: boolean;
  p_value?: number;
}

interface ArchGroup {
  architecture: string;
  experiments: ExperimentResult[];
  meanPassRate: number;
  meanIterations: number;
  meanDuration: number;
  acceptanceRate: number;
  bestPValue: number | null;
}

function groupByArchitecture(results: [number, ExperimentResult][]): ArchGroup[] {
  const map = new Map<string, ExperimentResult[]>();
  for (const [, r] of results) {
    const arch = r.config.workflow_architecture || "traditional";
    if (!map.has(arch)) map.set(arch, []);
    map.get(arch)!.push(r);
  }

  return Array.from(map.entries()).map(([architecture, experiments]) => {
    const n = experiments.length;
    const meanPassRate = experiments.reduce((s, e) => s + e.aggregate.pass_rate, 0) / n;
    const meanIterations = experiments.reduce((s, e) => s + e.aggregate.mean_iterations, 0) / n;
    const meanDuration = experiments.reduce((s, e) => s + e.aggregate.mean_duration_ms, 0) / n;
    const acceptanceRate = experiments.filter((e) => e.accepted).length / n;
    const pValues = experiments.map((e) => e.p_value).filter((p): p is number => p != null);
    const bestPValue = pValues.length > 0 ? Math.min(...pValues) : null;

    return { architecture, experiments, meanPassRate, meanIterations, meanDuration, acceptanceRate, bestPValue };
  });
}

const ARCH_LABELS: Record<string, string> = {
  traditional: "Traditional",
  agentic_verification: "Agentic Verification",
  multi_agent_pipeline: "Multi-Agent Pipeline",
};

export function ResultsTab() {
  const [results, setResults] = useState<[number, ExperimentResult][]>([]);
  const [loading, setLoading] = useState(true);

  const fetchResults = useCallback(async () => {
    try {
      const r = await invoke<[number, ExperimentResult][]>("get_autoresearch_results");
      setResults(r);
    } catch {
      // Campaign may not exist
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchResults();
    const id = setInterval(fetchResults, 5000);
    return () => clearInterval(id);
  }, [fetchResults]);

  if (loading) {
    return <div className="text-sm text-zinc-500 p-4">Loading results...</div>;
  }

  const groups = groupByArchitecture(results);

  if (groups.length === 0) {
    return (
      <div className="text-sm text-zinc-500 p-4">
        No results yet. Start a campaign in the Campaign tab.
      </div>
    );
  }

  // Find best values for highlighting
  const bestPassRate = Math.max(...groups.map((g) => g.meanPassRate));
  const bestIterations = Math.min(...groups.map((g) => g.meanIterations));
  const bestDuration = Math.min(...groups.map((g) => g.meanDuration));
  const bestAcceptance = Math.max(...groups.map((g) => g.acceptanceRate));

  function cellClass(value: number, best: number, lower = false): string {
    const isBest = lower ? value <= best : value >= best;
    return isBest ? "text-green-400 font-bold" : "text-zinc-300";
  }

  // Find recommendation: highest pass rate with most experiments
  const winner = [...groups].sort(
    (a, b) => b.meanPassRate - a.meanPassRate || b.experiments.length - a.experiments.length,
  )[0];

  return (
    <div className="p-4 space-y-6">
      {/* Comparison Table */}
      <div className="border border-zinc-700 rounded-lg overflow-hidden">
        <table className="w-full text-sm">
          <thead className="bg-zinc-800">
            <tr>
              <th className="text-left px-4 py-2 text-zinc-400">Metric</th>
              {groups.map((g) => (
                <th key={g.architecture} className="text-center px-4 py-2 text-zinc-300">
                  {ARCH_LABELS[g.architecture] ?? g.architecture}
                  <div className="text-xs text-zinc-500 font-normal">
                    {g.experiments.length} experiments
                  </div>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            <tr className="border-t border-zinc-800">
              <td className="px-4 py-2 text-zinc-400">Pass Rate</td>
              {groups.map((g) => (
                <td
                  key={g.architecture}
                  className={`px-4 py-2 text-center ${cellClass(g.meanPassRate, bestPassRate)}`}
                >
                  {(g.meanPassRate * 100).toFixed(1)}%
                </td>
              ))}
            </tr>
            <tr className="border-t border-zinc-800">
              <td className="px-4 py-2 text-zinc-400">Mean Iterations</td>
              {groups.map((g) => (
                <td
                  key={g.architecture}
                  className={`px-4 py-2 text-center ${cellClass(g.meanIterations, bestIterations, true)}`}
                >
                  {g.meanIterations.toFixed(1)}
                </td>
              ))}
            </tr>
            <tr className="border-t border-zinc-800">
              <td className="px-4 py-2 text-zinc-400">Mean Duration</td>
              {groups.map((g) => (
                <td
                  key={g.architecture}
                  className={`px-4 py-2 text-center ${cellClass(g.meanDuration, bestDuration, true)}`}
                >
                  {(g.meanDuration / 1000).toFixed(1)}s
                </td>
              ))}
            </tr>
            <tr className="border-t border-zinc-800">
              <td className="px-4 py-2 text-zinc-400">Acceptance Rate</td>
              {groups.map((g) => (
                <td
                  key={g.architecture}
                  className={`px-4 py-2 text-center ${cellClass(g.acceptanceRate, bestAcceptance)}`}
                >
                  {(g.acceptanceRate * 100).toFixed(1)}%
                </td>
              ))}
            </tr>
            <tr className="border-t border-zinc-800">
              <td className="px-4 py-2 text-zinc-400">Best p-value</td>
              {groups.map((g) => (
                <td key={g.architecture} className="px-4 py-2 text-center text-zinc-400">
                  {g.bestPValue != null ? g.bestPValue.toFixed(4) : "-"}
                </td>
              ))}
            </tr>
          </tbody>
        </table>
      </div>

      {/* Recommendation */}
      {winner && (
        <div className="border border-zinc-700 rounded-lg p-4 bg-zinc-800/30">
          <div className="text-sm font-bold text-zinc-200 mb-1">Recommendation</div>
          <div className="text-sm text-zinc-300">
            <span className="text-green-400 font-bold">
              {ARCH_LABELS[winner.architecture] ?? winner.architecture}
            </span>{" "}
            shows the best pass rate at{" "}
            <span className="font-mono">{(winner.meanPassRate * 100).toFixed(1)}%</span> across{" "}
            {winner.experiments.length} experiments.
            {winner.bestPValue != null && (
              <span className="text-zinc-400">
                {" "}
                Statistical significance: p={winner.bestPValue.toFixed(4)}
              </span>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

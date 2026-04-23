/**
 * ProgressTab — Shows meta-optimizer impact: baseline vs current metrics with deltas.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AgentEffectivenessPanel } from "./AgentEffectivenessPanel";
import { AgenticMetricsPanel } from "./AgenticMetricsPanel";
import { FailureAnalysisSection } from "./FailureAnalysisSection";
import { RegressionAlertBanner } from "./RegressionAlertBanner";
import { CanaryStatusPanel } from "./CanaryStatusPanel";
import { CostEffectivenessPanel } from "./CostEffectivenessPanel";
import { AgentInteractionMatrix } from "./AgentInteractionMatrix";
import { SpecComplianceSummaryTable } from "../specs/SpecCompliancePanel";

interface SnapshotMetrics {
  success_rate: number;
  avg_duration_secs: number;
  avg_iterations: number;
  avg_cost_cents: number;
  total_runs: number;
  successful_runs: number;
  failed_runs: number;
  partial_runs: number;
}

interface MetricsDelta {
  success_rate_delta: number;
  duration_delta: number;
  iterations_delta: number;
  cost_delta: number;
}

interface Snapshot {
  id: string;
  snapshot_type: string;
  period_start: string;
  period_end: string;
  metrics_json: string;
  breakdown_json: string | null;
  recommendation_id: string | null;
  runs_included: number;
  created_at: string;
}

interface ProgressSummary {
  baseline: SnapshotMetrics | null;
  current: SnapshotMetrics | null;
  delta: MetricsDelta | null;
  snapshots: Snapshot[];
  applied_recommendations_count: number;
}

interface BreakdownEntry {
  architecture?: string;
  count?: number;
  total_runs?: number;
  success_rate: number;
  avg_duration_secs: number;
  avg_iterations?: number;
}

const SNAPSHOT_TYPE_COLORS: Record<string, string> = {
  baseline: "text-blue-400 bg-blue-900/30",
  periodic: "text-zinc-400 bg-zinc-700/30",
  post_apply: "text-amber-400 bg-amber-900/30",
};

function formatRate(val: number): string {
  return `${(val * 100).toFixed(1)}%`;
}

function formatDuration(secs: number): string {
  return `${secs.toFixed(1)}s`;
}

function formatIterations(val: number): string {
  return val.toFixed(1);
}

function formatCost(cents: number): string {
  return `$${(cents / 100).toFixed(2)}`;
}

function DeltaIndicator({ delta, higherIsBetter }: { delta: number; higherIsBetter: boolean }) {
  if (Math.abs(delta) < 0.001) {
    return <span className="text-zinc-500 text-xs">--</span>;
  }

  const isPositive = delta > 0;
  const isImproved = higherIsBetter ? isPositive : !isPositive;
  const arrow = isPositive ? "\u2191" : "\u2193";
  const color = isImproved ? "text-green-400" : "text-red-400";

  return (
    <span className={`text-xs font-medium ${color}`}>
      {arrow} {Math.abs(delta).toFixed(2)}
    </span>
  );
}

function MetricCard({
  label,
  baseline,
  current,
  delta,
  higherIsBetter,
}: {
  label: string;
  baseline: string;
  current: string;
  delta: number;
  higherIsBetter: boolean;
}) {
  return (
    <div className="bg-zinc-800 rounded-lg p-4 border border-zinc-700">
      <div className="text-xs text-zinc-500 uppercase tracking-wide mb-3">{label}</div>
      <div className="flex items-end justify-between mb-2">
        <div>
          <div className="text-2xl font-semibold text-zinc-100">{current}</div>
          <div className="text-xs text-zinc-500 mt-1">
            Baseline: <span className="text-zinc-400">{baseline}</span>
          </div>
        </div>
        <DeltaIndicator delta={delta} higherIsBetter={higherIsBetter} />
      </div>
    </div>
  );
}

export function ProgressTab() {
  const [summary, setSummary] = useState<ProgressSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [capturing, setCapturing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [category, setCategory] = useState<string>("main");

  const load = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<ProgressSummary>("get_meta_optimizer_progress", {
        category,
      });
      setSummary(result);
    } catch (e) {
      console.error("Failed to load progress:", e);
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [category]);

  useEffect(() => {
    let cancelled = false;
    // Defer via microtask so the effect body itself doesn't synchronously
    // trigger setState inside load.
    void Promise.resolve().then(() => {
      if (!cancelled) void load();
    });
    return () => {
      cancelled = true;
    };
  }, [load]);

  const handleCaptureBaseline = async () => {
    try {
      setCapturing(true);
      await invoke("capture_meta_optimizer_baseline", {
        category,
      });
      await load();
    } catch (e) {
      console.error("Failed to capture baseline:", e);
      setError(String(e));
    } finally {
      setCapturing(false);
    }
  };

  if (loading) {
    return <div className="p-4 text-zinc-500 text-sm">Loading progress data...</div>;
  }

  if (error && !summary) {
    return <div className="p-4 text-red-400 text-sm">Failed to load progress: {error}</div>;
  }

  const hasBaseline = summary?.baseline != null;
  const hasCurrent = summary?.current != null;

  return (
    <div className="p-4 space-y-6">
      {/* Regression alert banner */}
      <RegressionAlertBanner />

      {/* Agentic quality metrics (deepeval-inspired) */}
      <AgenticMetricsPanel />

      {/* Active canary rollouts */}
      <CanaryStatusPanel />

      {/* Workflow category dropdown */}
      <div className="flex items-center gap-2 mb-4">
        <span className="text-xs text-zinc-500">Category:</span>
        <select
          value={category}
          onChange={(e) => setCategory(e.target.value)}
          className="bg-zinc-800 text-zinc-200 text-sm px-2 py-1 rounded border border-zinc-700"
        >
          <option value="main">Main Workflows</option>
          <option value="reflections">Reflections</option>
          <option value="fixers">Fixers</option>
          <option value="follow_ups">Follow-ups</option>
          <option value="meta_optimizer">Meta-Optimizer</option>
        </select>
      </div>

      {/* A. Baseline capture section */}
      <div className="bg-zinc-900 rounded-lg p-4 border border-zinc-800">
        {!hasBaseline ? (
          <div className="flex items-center justify-between">
            <div>
              <div className="text-zinc-200 font-medium">No baseline captured yet</div>
              <div className="text-zinc-500 text-sm mt-1">
                Capture a baseline snapshot to start tracking meta-optimizer impact. This records
                the current performance metrics from the last 30 days of workflow runs.
              </div>
            </div>
            <button
              onClick={handleCaptureBaseline}
              disabled={capturing}
              className="px-4 py-2 bg-blue-700 text-blue-100 rounded hover:bg-blue-600 disabled:opacity-50 whitespace-nowrap ml-4"
            >
              {capturing ? "Capturing..." : "Capture Baseline"}
            </button>
          </div>
        ) : (
          <div className="flex items-center justify-between">
            <div className="text-sm text-zinc-400">
              Baseline captured:{" "}
              <span className="text-zinc-300">
                {summary?.snapshots
                  .filter((s) => s.snapshot_type === "baseline")
                  .map((s) => new Date(s.created_at).toLocaleString())[0] ?? "unknown"}
              </span>
              {" | "}
              {summary?.baseline?.total_runs ?? 0} runs analyzed
            </div>
            <button
              onClick={handleCaptureBaseline}
              disabled={capturing}
              className="px-3 py-1.5 text-xs bg-zinc-700 text-zinc-300 rounded hover:bg-zinc-600 disabled:opacity-50"
            >
              {capturing ? "Capturing..." : "Recapture Baseline"}
            </button>
          </div>
        )}
      </div>

      {/* B. Metrics comparison cards */}
      {hasBaseline && hasCurrent && summary?.delta && (
        <div className="grid grid-cols-2 lg:grid-cols-4 gap-4">
          <MetricCard
            label="Success Rate"
            baseline={formatRate(summary.baseline!.success_rate)}
            current={formatRate(summary.current!.success_rate)}
            delta={summary.delta.success_rate_delta * 100}
            higherIsBetter={true}
          />
          <MetricCard
            label="Avg Duration"
            baseline={formatDuration(summary.baseline!.avg_duration_secs)}
            current={formatDuration(summary.current!.avg_duration_secs)}
            delta={summary.delta.duration_delta}
            higherIsBetter={false}
          />
          <MetricCard
            label="Avg Iterations"
            baseline={formatIterations(summary.baseline!.avg_iterations)}
            current={formatIterations(summary.current!.avg_iterations)}
            delta={summary.delta.iterations_delta}
            higherIsBetter={false}
          />
          <MetricCard
            label="Avg Cost"
            baseline={formatCost(summary.baseline!.avg_cost_cents)}
            current={formatCost(summary.current!.avg_cost_cents)}
            delta={summary.delta.cost_delta}
            higherIsBetter={false}
          />
        </div>
      )}

      {hasBaseline && !hasCurrent && (
        <div className="text-zinc-500 text-sm py-4 text-center">
          No current periodic snapshot available. Run some workflows and capture a periodic snapshot
          to see comparisons.
        </div>
      )}

      {!hasBaseline && hasCurrent && (
        <div className="bg-zinc-800 rounded-lg p-4 border border-zinc-700">
          <div className="text-xs text-zinc-500 uppercase tracking-wide mb-2">
            Current Metrics (last 30 days)
          </div>
          <div className="grid grid-cols-2 lg:grid-cols-4 gap-4 text-sm">
            <div>
              <span className="text-zinc-500">Success Rate: </span>
              <span className="text-zinc-200">{formatRate(summary!.current!.success_rate)}</span>
            </div>
            <div>
              <span className="text-zinc-500">Avg Duration: </span>
              <span className="text-zinc-200">
                {formatDuration(summary!.current!.avg_duration_secs)}
              </span>
            </div>
            <div>
              <span className="text-zinc-500">Avg Iterations: </span>
              <span className="text-zinc-200">
                {formatIterations(summary!.current!.avg_iterations)}
              </span>
            </div>
            <div>
              <span className="text-zinc-500">Avg Cost: </span>
              <span className="text-zinc-200">{formatCost(summary!.current!.avg_cost_cents)}</span>
            </div>
          </div>
        </div>
      )}

      {/* C. Applied recommendations count */}
      {summary && summary.applied_recommendations_count > 0 && (
        <div className="bg-zinc-800 rounded-lg px-4 py-3 border border-zinc-700 flex items-center gap-2">
          <span className="text-zinc-500 text-sm">Applied Recommendations:</span>
          <span className="text-zinc-200 font-medium">{summary.applied_recommendations_count}</span>
        </div>
      )}

      {/* Spec Compliance Overview */}
      <SpecComplianceSummaryTable />

      {/* E. Per-agent effectiveness */}
      <AgentEffectivenessPanel />

      {/* Cost-Effectiveness Pareto */}
      <CostEffectivenessPanel />

      {/* Agent Interaction Matrix */}
      <AgentInteractionMatrix />

      {/* F. Per-architecture breakdown */}
      <ArchitectureBreakdown snapshots={summary?.snapshots ?? []} />

      {/* D. Snapshot history table */}
      {summary && summary.snapshots.length > 0 && (
        <div>
          <div className="text-sm text-zinc-400 mb-2">Snapshot History</div>
          <table className="w-full text-sm">
            <thead>
              <tr className="text-zinc-500 text-xs border-b border-zinc-800">
                <th className="text-left py-2 px-3">Type</th>
                <th className="text-left py-2 px-3">Period</th>
                <th className="text-right py-2 px-3">Runs</th>
                <th className="text-right py-2 px-3">Success Rate</th>
                <th className="text-right py-2 px-3">Avg Duration</th>
                <th className="text-left py-2 px-3">Created</th>
              </tr>
            </thead>
            <tbody>
              {summary.snapshots.map((snap) => {
                const metrics = parseMetrics(snap.metrics_json);
                return (
                  <tr key={snap.id} className="border-b border-zinc-800/50 hover:bg-zinc-800/30">
                    <td className="py-2 px-3">
                      <span
                        className={`text-xs px-2 py-0.5 rounded ${SNAPSHOT_TYPE_COLORS[snap.snapshot_type] ?? ""}`}
                      >
                        {snap.snapshot_type}
                      </span>
                    </td>
                    <td className="py-2 px-3 text-zinc-400 text-xs">
                      {formatPeriod(snap.period_start, snap.period_end)}
                    </td>
                    <td className="py-2 px-3 text-right text-zinc-300">{snap.runs_included}</td>
                    <td className="py-2 px-3 text-right text-zinc-300">
                      {metrics ? formatRate(metrics.success_rate) : "--"}
                    </td>
                    <td className="py-2 px-3 text-right text-zinc-300">
                      {metrics ? formatDuration(metrics.avg_duration_secs) : "--"}
                    </td>
                    <td className="py-2 px-3 text-zinc-500 text-xs">
                      {new Date(snap.created_at).toLocaleString()}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {summary && summary.snapshots.length === 0 && (
        <div className="text-zinc-500 text-sm py-4 text-center">No snapshots recorded yet.</div>
      )}

      {/* Failure Analysis */}
      <div className="border-t border-zinc-800 pt-4 mt-6">
        <FailureAnalysisSection category={category} />
      </div>

      {/* Refresh button */}
      <div className="flex justify-end">
        <button onClick={load} className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1">
          Refresh
        </button>
      </div>
    </div>
  );
}

function parseMetrics(json: string): SnapshotMetrics | null {
  try {
    return JSON.parse(json) as SnapshotMetrics;
  } catch {
    return null;
  }
}

function formatPeriod(start: string, end: string): string {
  try {
    const s = new Date(start).toLocaleDateString();
    const e = new Date(end).toLocaleDateString();
    return `${s} - ${e}`;
  } catch {
    return `${start} - ${end}`;
  }
}

function ArchitectureBreakdown({ snapshots }: { snapshots: Snapshot[] }) {
  // Find the most recent snapshot with breakdown data
  const snapshotWithBreakdown = snapshots.find(
    (s) => s.breakdown_json && s.breakdown_json !== "{}" && s.breakdown_json !== "null",
  );

  if (!snapshotWithBreakdown?.breakdown_json) {
    return null;
  }

  let entries: { architecture: string; data: BreakdownEntry }[];
  try {
    const parsed = JSON.parse(snapshotWithBreakdown.breakdown_json);

    if (Array.isArray(parsed)) {
      // Array format: [{ architecture, count, success_rate, avg_duration_secs }]
      entries = parsed.map((item: BreakdownEntry) => ({
        architecture: item.architecture ?? "unknown",
        data: item,
      }));
    } else if (typeof parsed === "object" && parsed !== null) {
      // Object format: { "arch_name": { total_runs, success_rate, avg_duration_secs } }
      entries = Object.entries(parsed).map(([arch, data]) => ({
        architecture: arch,
        data: data as BreakdownEntry,
      }));
    } else {
      return null;
    }
  } catch {
    return null;
  }

  if (entries.length === 0) {
    return null;
  }

  return (
    <div>
      <div className="text-sm text-zinc-400 mb-2">Per-Architecture Breakdown</div>
      <table className="w-full text-sm">
        <thead>
          <tr className="text-zinc-500 text-xs border-b border-zinc-800">
            <th className="text-left py-2 px-3">Architecture</th>
            <th className="text-right py-2 px-3">Runs</th>
            <th className="text-right py-2 px-3">Success Rate</th>
            <th className="text-right py-2 px-3">Avg Duration</th>
          </tr>
        </thead>
        <tbody>
          {entries.map(({ architecture, data }) => (
            <tr key={architecture} className="border-b border-zinc-800/50 hover:bg-zinc-800/30">
              <td className="py-2 px-3 text-zinc-200">{architecture}</td>
              <td className="py-2 px-3 text-right text-zinc-300">
                {data.count ?? data.total_runs ?? "--"}
              </td>
              <td className="py-2 px-3 text-right text-zinc-300">
                {data.success_rate != null ? `${data.success_rate.toFixed(1)}%` : "--"}
              </td>
              <td className="py-2 px-3 text-right text-zinc-300">
                {data.avg_duration_secs != null ? formatDuration(data.avg_duration_secs) : "--"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

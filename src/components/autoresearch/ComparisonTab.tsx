/**
 * ComparisonTab -- launch the same workflow with different architectures
 * and compare results side-by-side.
 */

import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface Workflow {
  id: string;
  name: string;
}

interface ComparisonEntry {
  label: string;
  overrides: Record<string, unknown>;
  task_run_id: string | null;
  status: string; // pending | running | completed | failed
  result?: {
    success: boolean;
    iterations: number;
    duration_ms: number;
  };
}

interface ComparisonRun {
  id: string;
  workflow_id: string;
  variation_type: string;
  status: string; // running | completed | failed
  entries: ComparisonEntry[];
  report: string | null;
  created_at: string;
  completed_at: string | null;
}

// ---------------------------------------------------------------------------
// Status badge styles
// ---------------------------------------------------------------------------

const statusStyle: Record<string, string> = {
  pending: "bg-zinc-800 text-zinc-400 border-zinc-700",
  running: "bg-blue-900/50 text-blue-300 border-blue-700",
  completed: "bg-green-900/50 text-green-300 border-green-700",
  failed: "bg-red-900/50 text-red-300 border-red-700",
};

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function ComparisonTab() {
  // Launch form state
  const [workflows, setWorkflows] = useState<Workflow[]>([]);
  const [selectedWorkflow, setSelectedWorkflow] = useState("");
  const [variationType, setVariationType] = useState<"architecture" | "same" | "custom">(
    "architecture",
  );
  const [useWorktree, setUseWorktree] = useState(true);
  const [launching, setLaunching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Results state
  const [comparisons, setComparisons] = useState<ComparisonRun[]>([]);
  const [activeComparison, setActiveComparison] = useState<string | null>(null);
  const [activeData, setActiveData] = useState<ComparisonRun | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Fetch saved workflows on mount
  useEffect(() => {
    (async () => {
      try {
        const wfs = await invoke<Workflow[]>("list_unified_workflows");
        setWorkflows(wfs);
        if (wfs.length > 0 && !selectedWorkflow) {
          setSelectedWorkflow(wfs[0].id);
        }
      } catch {
        // Command may not be available
      }
    })();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Fetch comparison list
  const fetchComparisons = useCallback(async () => {
    try {
      const list = await invoke<ComparisonRun[]>("list_comparisons");
      setComparisons(list);
    } catch {
      // ignore
    }
  }, []);

  useEffect(() => {
    fetchComparisons();
  }, [fetchComparisons]);

  // Poll active comparison status
  useEffect(() => {
    if (pollRef.current) clearInterval(pollRef.current);
    if (!activeComparison) {
      setActiveData(null);
      return;
    }

    const poll = async () => {
      try {
        const data = await invoke<ComparisonRun>("get_comparison_status", {
          comparisonId: activeComparison,
        });
        setActiveData(data);
        if (data.status !== "running") {
          // Done polling
          if (pollRef.current) clearInterval(pollRef.current);
          fetchComparisons();
        }
      } catch {
        // ignore
      }
    };

    poll();
    pollRef.current = setInterval(poll, 5000);
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [activeComparison, fetchComparisons]);

  // Launch handler
  async function handleStart() {
    if (!selectedWorkflow) return;
    setLaunching(true);
    setError(null);
    try {
      const compId = await invoke<string>("start_comparison", {
        workflowId: selectedWorkflow,
        variationType: variationType,
        useWorktree: useWorktree,
      });
      setActiveComparison(compId);
      fetchComparisons();
    } catch (e) {
      setError(`Failed to start comparison: ${e}`);
    } finally {
      setLaunching(false);
    }
  }

  // Format duration
  function fmtDuration(ms: number): string {
    if (ms < 1000) return `${ms}ms`;
    const secs = Math.round(ms / 1000);
    if (secs < 60) return `${secs}s`;
    const mins = Math.floor(secs / 60);
    const rem = secs % 60;
    return `${mins}m ${rem}s`;
  }

  return (
    <div className="p-4 space-y-6 text-zinc-200">
      {/* ── Launch Section ── */}
      <div className="border border-zinc-700 rounded-lg p-4 bg-zinc-900/50">
        <h3 className="text-sm font-semibold text-zinc-100 mb-3">Launch Comparison</h3>

        <div className="flex flex-wrap items-end gap-4">
          {/* Workflow selector */}
          <div className="flex flex-col gap-1">
            <label className="text-xs text-zinc-400">Workflow</label>
            <select
              className="bg-zinc-800 border border-zinc-700 rounded px-2 py-1.5 text-sm text-zinc-200 min-w-[200px]"
              value={selectedWorkflow}
              onChange={(e) => setSelectedWorkflow(e.target.value)}
            >
              {workflows.length === 0 && <option value="">No workflows</option>}
              {workflows.map((w) => (
                <option key={w.id} value={w.id}>
                  {w.name}
                </option>
              ))}
            </select>
          </div>

          {/* Variation type buttons */}
          <div className="flex flex-col gap-1">
            <label className="text-xs text-zinc-400">Compare</label>
            <div className="flex gap-1">
              {(
                [
                  ["architecture", "Architectures"],
                  ["same", "Repeatability"],
                ] as const
              ).map(([val, label]) => (
                <button
                  key={val}
                  onClick={() => setVariationType(val)}
                  className={`px-3 py-1.5 text-xs rounded border transition-colors ${
                    variationType === val
                      ? "bg-indigo-700 border-indigo-600 text-white"
                      : "bg-zinc-800 border-zinc-700 text-zinc-400 hover:text-zinc-200"
                  }`}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>

          {/* Worktree checkbox */}
          <label className="flex items-center gap-2 text-xs text-zinc-400 cursor-pointer select-none pb-0.5">
            <input
              type="checkbox"
              checked={useWorktree}
              onChange={(e) => setUseWorktree(e.target.checked)}
              className="accent-indigo-500"
            />
            Worktree isolation
          </label>

          {/* Start button */}
          <button
            onClick={handleStart}
            disabled={launching || !selectedWorkflow}
            className="px-4 py-1.5 text-sm rounded bg-indigo-600 hover:bg-indigo-500 disabled:opacity-40 text-white transition-colors"
          >
            {launching ? "Launching..." : "Start Comparison"}
          </button>
        </div>

        {error && <p className="mt-2 text-xs text-red-400">{error}</p>}
      </div>

      {/* ── Active Comparison Detail ── */}
      {activeData && (
        <div className="border border-zinc-700 rounded-lg p-4 bg-zinc-900/50">
          <div className="flex items-center justify-between mb-3">
            <h3 className="text-sm font-semibold text-zinc-100">Active Comparison</h3>
            <span
              className={`px-2 py-0.5 text-[11px] rounded border ${
                statusStyle[activeData.status] || statusStyle.pending
              }`}
            >
              {activeData.status}
            </span>
          </div>

          {/* Entries table */}
          <table className="w-full text-xs">
            <thead>
              <tr className="text-zinc-500 border-b border-zinc-800">
                <th className="text-left py-1 pr-4">Architecture</th>
                <th className="text-left py-1 pr-4">Status</th>
                <th className="text-right py-1 pr-4">Iterations</th>
                <th className="text-right py-1 pr-4">Duration</th>
                <th className="text-center py-1">Success</th>
              </tr>
            </thead>
            <tbody>
              {activeData.entries.map((entry, i) => (
                <tr key={`${entry.label}-${i}`} className="border-b border-zinc-800/50">
                  <td className="py-1.5 pr-4 text-zinc-200">{entry.label}</td>
                  <td className="py-1.5 pr-4">
                    <span
                      className={`px-1.5 py-0.5 rounded border text-[10px] ${
                        statusStyle[entry.status] || statusStyle.pending
                      }`}
                    >
                      {entry.status}
                    </span>
                  </td>
                  <td className="py-1.5 pr-4 text-right text-zinc-300">
                    {entry.result ? entry.result.iterations : "-"}
                  </td>
                  <td className="py-1.5 pr-4 text-right text-zinc-300">
                    {entry.result ? fmtDuration(entry.result.duration_ms) : "-"}
                  </td>
                  <td className="py-1.5 text-center">
                    {entry.result ? (
                      entry.result.success ? (
                        <span className="text-green-400">Pass</span>
                      ) : (
                        <span className="text-red-400">Fail</span>
                      )
                    ) : (
                      "-"
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          {/* Structured comparison summary */}
          {activeData.entries.some((e) => e.result) && (
            <div className="mt-3">
              <div className="text-xs text-zinc-500 mb-1">Structured Comparison</div>
              <table className="w-full text-xs border border-zinc-700 rounded overflow-hidden">
                <thead>
                  <tr className="text-zinc-400 bg-zinc-800 border-b border-zinc-700">
                    <th className="text-left py-1.5 px-3">Variant</th>
                    <th className="text-center py-1.5 px-3">Success</th>
                    <th className="text-right py-1.5 px-3">Iterations</th>
                    <th className="text-right py-1.5 px-3">Duration</th>
                  </tr>
                </thead>
                <tbody>
                  {activeData.entries
                    .filter((e) => e.result)
                    .map((entry, i) => (
                      <tr
                        key={`summary-${entry.label}-${i}`}
                        className="border-b border-zinc-800/50"
                      >
                        <td className="py-1.5 px-3 text-zinc-200">{entry.label}</td>
                        <td className="py-1.5 px-3 text-center">
                          {entry.result!.success ? (
                            <span className="text-green-400">&#10003;</span>
                          ) : (
                            <span className="text-red-400">&#10007;</span>
                          )}
                        </td>
                        <td className="py-1.5 px-3 text-right text-zinc-300">
                          {entry.result!.iterations}
                        </td>
                        <td className="py-1.5 px-3 text-right text-zinc-300">
                          {fmtDuration(entry.result!.duration_ms)}
                        </td>
                      </tr>
                    ))}
                </tbody>
              </table>
            </div>
          )}

          {activeData.report && (
            <div className="mt-3 p-2 bg-zinc-800 rounded text-xs text-zinc-300 whitespace-pre-wrap">
              {activeData.report}
            </div>
          )}
        </div>
      )}

      {/* ── History ── */}
      {comparisons.length > 0 && (
        <div className="border border-zinc-700 rounded-lg p-4 bg-zinc-900/50">
          <h3 className="text-sm font-semibold text-zinc-100 mb-3">Recent Comparisons</h3>
          <table className="w-full text-xs">
            <thead>
              <tr className="text-zinc-500 border-b border-zinc-800">
                <th className="text-left py-1 pr-4">ID</th>
                <th className="text-left py-1 pr-4">Type</th>
                <th className="text-left py-1 pr-4">Status</th>
                <th className="text-left py-1 pr-4">Entries</th>
                <th className="text-left py-1">Created</th>
              </tr>
            </thead>
            <tbody>
              {comparisons.map((c) => (
                <tr
                  key={c.id}
                  className={`border-b border-zinc-800/50 cursor-pointer hover:bg-zinc-800/40 ${
                    activeComparison === c.id ? "bg-zinc-800/60" : ""
                  }`}
                  onClick={() => setActiveComparison(c.id)}
                >
                  <td className="py-1.5 pr-4 text-zinc-400 font-mono">{c.id.slice(0, 12)}...</td>
                  <td className="py-1.5 pr-4 text-zinc-300">{c.variation_type}</td>
                  <td className="py-1.5 pr-4">
                    <span
                      className={`px-1.5 py-0.5 rounded border text-[10px] ${
                        statusStyle[c.status] || statusStyle.pending
                      }`}
                    >
                      {c.status}
                    </span>
                  </td>
                  <td className="py-1.5 pr-4 text-zinc-400">{c.entries.length} entries</td>
                  <td className="py-1.5 text-zinc-400">
                    {new Date(c.created_at).toLocaleString()}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

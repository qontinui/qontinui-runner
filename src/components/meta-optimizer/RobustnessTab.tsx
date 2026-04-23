/**
 * RobustnessTab — Robustness test reports and golden dataset management.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface RobustnessReport {
  id: string;
  prompt_variant_id: string | null;
  recommendation_id: string | null;
  total_tests: number;
  passed: number;
  failed: number;
  category_breakdown: { category: string; total: number; passed: number; failed: number }[];
  failures: {
    category: string;
    description: string;
    input: string;
    expected: string;
    actual: string;
  }[];
  created_at: string;
}

interface GoldenDataset {
  id: string;
  agent_type: string;
  name: string;
  entries: {
    input_hash: string;
    input_summary: string;
    expected_success: boolean;
    source_task_run_id: string | null;
  }[];
  created_at: string;
  updated_at: string;
}

const CATEGORY_LABELS: Record<string, string> = {
  edge_case: "Edge Case",
  ambiguous: "Ambiguous",
  domain_boundary: "Domain Boundary",
  adversarial_format: "Adversarial Format",
  regression_suite: "Regression Suite",
};

export function RobustnessTab() {
  const [reports, setReports] = useState<RobustnessReport[]>([]);
  const [datasets, setDatasets] = useState<GoldenDataset[]>([]);
  const [loadingReports, setLoadingReports] = useState(true);
  const [loadingDatasets, setLoadingDatasets] = useState(true);
  const [runningTest, setRunningTest] = useState(false);
  const [buildingDataset, setBuildingDataset] = useState(false);
  const [agentType, setAgentType] = useState<string>("researcher");
  const [expandedReport, setExpandedReport] = useState<string | null>(null);
  const [expandedDataset, setExpandedDataset] = useState<string | null>(null);

  const loadReports = useCallback(async () => {
    try {
      setLoadingReports(true);
      const result = await invoke<RobustnessReport[]>("get_robustness_reports", {});
      setReports(result);
    } catch (e) {
      console.error("Failed to load robustness reports:", e);
    } finally {
      setLoadingReports(false);
    }
  }, []);

  const loadDatasets = useCallback(async () => {
    try {
      setLoadingDatasets(true);
      const result = await invoke<GoldenDataset[]>("get_golden_datasets", { agentType });
      setDatasets(result);
    } catch (e) {
      console.error("Failed to load golden datasets:", e);
    } finally {
      setLoadingDatasets(false);
    }
  }, [agentType]);

  useEffect(() => {
    // Async IIFE: avoid invoking setState-bearing callbacks synchronously
    // in the effect body (set-state-in-effect).
    let cancelled = false;
    void (async () => {
      if (cancelled) return;
      await loadReports();
    })();
    return () => {
      cancelled = true;
    };
  }, [loadReports]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      if (cancelled) return;
      await loadDatasets();
    })();
    return () => {
      cancelled = true;
    };
  }, [loadDatasets]);

  const handleRunTest = useCallback(async () => {
    try {
      setRunningTest(true);
      await invoke<RobustnessReport>("run_robustness_test", { agentType });
      await loadReports();
    } catch (e) {
      console.error("Failed to run robustness test:", e);
    } finally {
      setRunningTest(false);
    }
  }, [agentType, loadReports]);

  const handleBuildDataset = useCallback(async () => {
    try {
      setBuildingDataset(true);
      await invoke<GoldenDataset>("build_golden_dataset", { agentType });
      await loadDatasets();
    } catch (e) {
      console.error("Failed to build golden dataset:", e);
    } finally {
      setBuildingDataset(false);
    }
  }, [agentType, loadDatasets]);

  return (
    <div className="p-4 space-y-6">
      {/* Agent type selector */}
      <div className="flex gap-3 items-center">
        <select
          className="bg-zinc-800 text-zinc-200 text-sm px-2 py-1 rounded border border-zinc-700"
          value={agentType}
          onChange={(e) => setAgentType(e.target.value)}
        >
          <option value="researcher">Researcher</option>
          <option value="coder">Coder</option>
          <option value="planner">Planner</option>
          <option value="verifier">Verifier</option>
          <option value="orchestrator">Orchestrator</option>
        </select>
      </div>

      {/* Robustness Reports Section */}
      <div className="space-y-3">
        <div className="flex items-center gap-3">
          <h3 className="text-sm text-zinc-300 font-medium">Robustness Reports</h3>
          <button
            onClick={handleRunTest}
            disabled={runningTest}
            className="px-3 py-1 text-xs bg-blue-800 text-blue-200 rounded hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {runningTest ? "Running..." : "Run Test"}
          </button>
          <button
            onClick={loadReports}
            className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1"
          >
            Refresh
          </button>
          <span className="text-xs text-zinc-500 ml-auto">{reports.length} reports</span>
        </div>

        {loadingReports ? (
          <div className="text-zinc-500 text-sm">Loading...</div>
        ) : reports.length === 0 ? (
          <div className="text-zinc-500 text-sm py-4 text-center">
            No robustness reports yet. Select an agent type and click "Run Test".
          </div>
        ) : (
          <div className="space-y-2">
            {reports.map((report) => {
              const passRate =
                report.total_tests > 0
                  ? ((report.passed / report.total_tests) * 100).toFixed(1)
                  : "0.0";
              return (
                <div
                  key={report.id}
                  className="bg-zinc-900 border border-zinc-800 rounded-lg overflow-hidden"
                >
                  <div
                    className="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-zinc-800/50"
                    onClick={() =>
                      setExpandedReport(expandedReport === report.id ? null : report.id)
                    }
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        setExpandedReport(expandedReport === report.id ? null : report.id);
                      }
                    }}
                    role="button"
                    tabIndex={0}
                  >
                    <span
                      className={`text-xs px-2 py-0.5 rounded ${
                        report.failed === 0
                          ? "text-green-400 bg-green-900/30"
                          : "text-red-400 bg-red-900/30"
                      }`}
                    >
                      {passRate}% pass
                    </span>
                    <span className="text-sm text-zinc-200 flex-1">
                      {report.passed}/{report.total_tests} passed
                      {report.failed > 0 && (
                        <span className="text-red-400 ml-2">({report.failed} failed)</span>
                      )}
                    </span>
                    <span className="text-xs text-zinc-600">
                      {new Date(report.created_at).toLocaleDateString()}
                    </span>
                  </div>

                  {expandedReport === report.id && (
                    <div className="px-4 pb-4 space-y-3 border-t border-zinc-800">
                      {/* Category breakdown */}
                      {report.category_breakdown.length > 0 && (
                        <div className="mt-3">
                          <div className="text-xs text-zinc-500 mb-2">Category Breakdown</div>
                          <table className="w-full text-xs">
                            <thead>
                              <tr className="text-zinc-500">
                                <th className="text-left py-1">Category</th>
                                <th className="text-right py-1">Total</th>
                                <th className="text-right py-1">Passed</th>
                                <th className="text-right py-1">Failed</th>
                                <th className="text-right py-1">Rate</th>
                              </tr>
                            </thead>
                            <tbody>
                              {report.category_breakdown.map((cat) => (
                                <tr key={cat.category} className="border-t border-zinc-700/50">
                                  <td className="py-1 text-zinc-300">
                                    {CATEGORY_LABELS[cat.category] || cat.category}
                                  </td>
                                  <td className="py-1 text-right text-zinc-400">{cat.total}</td>
                                  <td className="py-1 text-right text-green-400">{cat.passed}</td>
                                  <td className="py-1 text-right text-red-400">{cat.failed}</td>
                                  <td className="py-1 text-right text-zinc-300">
                                    {cat.total > 0
                                      ? ((cat.passed / cat.total) * 100).toFixed(0)
                                      : 0}
                                    %
                                  </td>
                                </tr>
                              ))}
                            </tbody>
                          </table>
                        </div>
                      )}

                      {/* Failures detail */}
                      {report.failures.length > 0 && (
                        <div>
                          <div className="text-xs text-zinc-500 mb-2">Failures</div>
                          <div className="space-y-2 max-h-64 overflow-auto">
                            {report.failures.map((f, i) => (
                              <div key={i} className="bg-zinc-800/50 rounded p-2 space-y-1">
                                <div className="flex items-center gap-2">
                                  <span className="text-xs px-1.5 py-0.5 rounded bg-red-900/30 text-red-400">
                                    {CATEGORY_LABELS[f.category] || f.category}
                                  </span>
                                  <span className="text-xs text-zinc-300">{f.description}</span>
                                </div>
                                <div className="text-xs text-zinc-500">
                                  Input: <span className="text-zinc-400 font-mono">{f.input}</span>
                                </div>
                                <div className="text-xs text-zinc-500">
                                  Expected:{" "}
                                  <span className="text-green-400 font-mono">{f.expected}</span>
                                </div>
                                <div className="text-xs text-zinc-500">
                                  Actual: <span className="text-red-400 font-mono">{f.actual}</span>
                                </div>
                              </div>
                            ))}
                          </div>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Golden Datasets Section */}
      <div className="space-y-3">
        <div className="flex items-center gap-3">
          <h3 className="text-sm text-zinc-300 font-medium">Golden Datasets</h3>
          <button
            onClick={handleBuildDataset}
            disabled={buildingDataset}
            className="px-3 py-1 text-xs bg-yellow-800 text-yellow-200 rounded hover:bg-yellow-700 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {buildingDataset ? "Building..." : "Build from History"}
          </button>
          <button
            onClick={loadDatasets}
            className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1"
          >
            Refresh
          </button>
          <span className="text-xs text-zinc-500 ml-auto">{datasets.length} datasets</span>
        </div>

        {loadingDatasets ? (
          <div className="text-zinc-500 text-sm">Loading...</div>
        ) : datasets.length === 0 ? (
          <div className="text-zinc-500 text-sm py-4 text-center">
            No golden datasets yet. Click "Build from History" to create one from past runs.
          </div>
        ) : (
          <div className="space-y-2">
            {datasets.map((ds) => (
              <div
                key={ds.id}
                className="bg-zinc-900 border border-zinc-800 rounded-lg overflow-hidden"
              >
                <div
                  className="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-zinc-800/50"
                  onClick={() => setExpandedDataset(expandedDataset === ds.id ? null : ds.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      setExpandedDataset(expandedDataset === ds.id ? null : ds.id);
                    }
                  }}
                  role="button"
                  tabIndex={0}
                >
                  <span className="text-xs px-2 py-0.5 rounded bg-yellow-900/30 text-yellow-400">
                    {ds.agent_type}
                  </span>
                  <span className="text-sm text-zinc-200 flex-1">{ds.name}</span>
                  <span className="text-xs text-zinc-500">
                    {ds.entries.length} entr{ds.entries.length !== 1 ? "ies" : "y"}
                  </span>
                  <span className="text-xs text-zinc-600">
                    {new Date(ds.created_at).toLocaleDateString()}
                  </span>
                </div>

                {expandedDataset === ds.id && (
                  <div className="px-4 pb-4 border-t border-zinc-800">
                    <div className="text-xs text-zinc-500 mt-3 mb-2">Entries</div>
                    <div className="space-y-1 max-h-48 overflow-auto">
                      {ds.entries.map((entry, i) => (
                        <div
                          key={i}
                          className="flex items-center gap-2 text-xs bg-zinc-800/50 rounded px-2 py-1"
                        >
                          <span
                            className={`px-1.5 py-0.5 rounded ${
                              entry.expected_success
                                ? "bg-green-900/30 text-green-400"
                                : "bg-red-900/30 text-red-400"
                            }`}
                          >
                            {entry.expected_success ? "pass" : "fail"}
                          </span>
                          <span className="text-zinc-300 flex-1 truncate">
                            {entry.input_summary}
                          </span>
                          <span className="text-zinc-600 font-mono text-[10px]">
                            {entry.input_hash.slice(0, 8)}
                          </span>
                        </div>
                      ))}
                    </div>
                    <div className="text-xs text-zinc-600 mt-2">
                      Updated: {new Date(ds.updated_at).toLocaleString()}
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

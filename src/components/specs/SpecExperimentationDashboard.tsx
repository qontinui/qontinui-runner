/**
 * SpecExperimentationDashboard — Unified dashboard for spec compliance and accuracy.
 *
 * Sections:
 * 1. Compliance Overview (grid with scores, sparklines)
 * 2. Accuracy Scores (element coverage, cross-page, mutation, freshness)
 * 3. Coverage Gaps (uncovered elements)
 * 4. Cross-Page Heatmap
 * 5. Freshness Alerts
 */

import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SpecComplianceSummaryTable } from "./SpecCompliancePanel";

interface SpecAccuracyRecord {
  id: string;
  spec_id: string;
  analysis_type: string;
  score: number;
  detail_json: string;
  created_at: string;
}

interface ElementCoverageResult {
  spec_target_coverage: number;
  element_coverage: number;
  total_elements: number;
  interactive_elements: number;
  elements_with_assertions: number;
  unmatched_targets: string[];
  uncovered_elements: string[];
}

type FreshnessEntry = [
  string, // spec name
  {
    is_stale: boolean;
    spec_modified: string | null;
    newest_component: string | null;
    staleness_days: number;
    component_files_checked: number;
  },
];

function scoreColor(score: number): string {
  if (score >= 0.9) return "text-green-400";
  if (score >= 0.7) return "text-yellow-400";
  return "text-red-400";
}

function scoreBadge(score: number): string {
  if (score >= 0.9) return "bg-green-900/30 text-green-400 border-green-800";
  if (score >= 0.7) return "bg-yellow-900/30 text-yellow-400 border-yellow-800";
  return "bg-red-900/30 text-red-400 border-red-800";
}

export function SpecExperimentationDashboard() {
  const [accuracyRecords, setAccuracyRecords] = useState<SpecAccuracyRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<"compliance" | "accuracy" | "coverage" | "freshness">(
    "compliance",
  );

  useEffect(() => {
    invoke<SpecAccuracyRecord[]>("get_spec_accuracy_results", {
      specId: null,
      analysisType: null,
    })
      .then(setAccuracyRecords)
      .catch((e) => console.error("Failed to load accuracy records:", e))
      .finally(() => setLoading(false));
  }, []);

  // Group accuracy records by spec_id and analysis_type for display
  const latestBySpecAndType = new Map<string, SpecAccuracyRecord>();
  for (const record of accuracyRecords) {
    const key = `${record.spec_id}::${record.analysis_type}`;
    const existing = latestBySpecAndType.get(key);
    if (!existing || record.created_at > existing.created_at) {
      latestBySpecAndType.set(key, record);
    }
  }

  const specIds = [...new Set(accuracyRecords.map((r) => r.spec_id))];
  const analysisTypes = [...new Set(accuracyRecords.map((r) => r.analysis_type))];

  return (
    <div className="p-4 space-y-6">
      {/* Tab navigation */}
      <div className="flex gap-1 border-b border-zinc-700 pb-1">
        {(
          [
            ["compliance", "Compliance"],
            ["accuracy", "Accuracy Scores"],
            ["coverage", "Coverage Gaps"],
            ["freshness", "Freshness"],
          ] as const
        ).map(([key, label]) => (
          <button
            key={key}
            onClick={() => setActiveTab(key)}
            className={`px-3 py-1.5 text-sm rounded-t ${
              activeTab === key
                ? "bg-zinc-700 text-zinc-100"
                : "text-zinc-500 hover:text-zinc-300"
            }`}
          >
            {label}
          </button>
        ))}
      </div>

      {/* Tab content */}
      {activeTab === "compliance" && (
        <div className="space-y-4">
          <SpecComplianceSummaryTable />
        </div>
      )}

      {activeTab === "accuracy" && (
        <div className="space-y-4">
          {loading ? (
            <div className="text-zinc-500 text-sm">Loading accuracy data...</div>
          ) : specIds.length === 0 ? (
            <div className="text-zinc-500 text-sm">
              No accuracy analysis results yet. Run element coverage, mutation testing, or other
              accuracy analyses to populate this view.
            </div>
          ) : (
            <div className="bg-zinc-800 rounded-lg border border-zinc-700 overflow-hidden">
              <table className="w-full text-sm">
                <thead>
                  <tr className="text-zinc-500 text-xs border-b border-zinc-700">
                    <th className="text-left py-2 px-3">Spec</th>
                    {analysisTypes.map((type) => (
                      <th key={type} className="text-right py-2 px-3">
                        {type.replace("_", " ")}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {specIds.map((specId) => (
                    <tr key={specId} className="border-b border-zinc-800/50 hover:bg-zinc-800/30">
                      <td className="py-2 px-3 text-zinc-300 truncate max-w-[200px]">
                        {specId}
                      </td>
                      {analysisTypes.map((type) => {
                        const record = latestBySpecAndType.get(`${specId}::${type}`);
                        return (
                          <td key={type} className="py-2 px-3 text-right">
                            {record ? (
                              <span
                                className={`px-2 py-0.5 rounded text-xs border ${scoreBadge(record.score)}`}
                              >
                                {(record.score * 100).toFixed(0)}%
                              </span>
                            ) : (
                              <span className="text-zinc-600 text-xs">--</span>
                            )}
                          </td>
                        );
                      })}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}

      {activeTab === "coverage" && <CoverageGapsView records={accuracyRecords} />}

      {activeTab === "freshness" && <FreshnessView />}
    </div>
  );
}

function CoverageGapsView({ records }: { records: SpecAccuracyRecord[] }) {
  const coverageRecords = records.filter((r) => r.analysis_type === "element_coverage");

  if (coverageRecords.length === 0) {
    return (
      <div className="text-zinc-500 text-sm">
        No element coverage analyses run yet. Use the element coverage analyzer on a spec to see
        coverage gaps.
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {coverageRecords.slice(0, 10).map((record) => {
        let detail: ElementCoverageResult | null = null;
        try {
          detail = JSON.parse(record.detail_json);
        } catch {
          /* ignore */
        }
        if (!detail) return null;

        return (
          <div
            key={record.id}
            className="bg-zinc-800 rounded-lg p-4 border border-zinc-700 space-y-3"
          >
            <div className="flex items-center justify-between">
              <span className="text-zinc-200 font-medium">{record.spec_id}</span>
              <span className={`text-sm ${scoreColor(detail.element_coverage)}`}>
                {(detail.element_coverage * 100).toFixed(0)}% element coverage
              </span>
            </div>

            <div className="grid grid-cols-3 gap-4 text-xs text-zinc-400">
              <div>
                Total elements: <span className="text-zinc-200">{detail.total_elements}</span>
              </div>
              <div>
                Interactive:{" "}
                <span className="text-zinc-200">{detail.interactive_elements}</span>
              </div>
              <div>
                With assertions:{" "}
                <span className="text-zinc-200">{detail.elements_with_assertions}</span>
              </div>
            </div>

            {detail.uncovered_elements.length > 0 && (
              <div>
                <div className="text-xs text-zinc-500 mb-1">
                  Uncovered interactive elements ({detail.uncovered_elements.length}):
                </div>
                <div className="flex flex-wrap gap-1">
                  {detail.uncovered_elements.slice(0, 20).map((el, i) => (
                    <span
                      key={i}
                      className="text-xs bg-zinc-700 text-zinc-300 px-2 py-0.5 rounded"
                    >
                      {el}
                    </span>
                  ))}
                  {detail.uncovered_elements.length > 20 && (
                    <span className="text-xs text-zinc-500">
                      +{detail.uncovered_elements.length - 20} more
                    </span>
                  )}
                </div>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

function FreshnessView() {
  const [results, setResults] = useState<FreshnessEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const runAnalysis = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<FreshnessEntry[]>("analyze_spec_freshness", {
        specsDir: "src/specs",
        componentsDir: "src/components",
      });
      setResults(result);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div className="text-sm text-zinc-400">
          Checks whether spec files are newer than their corresponding components.
        </div>
        <button
          onClick={runAnalysis}
          disabled={loading}
          className="px-3 py-1.5 text-sm bg-zinc-700 text-zinc-200 rounded hover:bg-zinc-600 disabled:opacity-50"
        >
          {loading ? "Analyzing..." : "Run Freshness Check"}
        </button>
      </div>

      {error && <div className="text-red-400 text-sm">{error}</div>}

      {results.length > 0 && (
        <div className="bg-zinc-800 rounded-lg border border-zinc-700 overflow-hidden">
          <table className="w-full text-sm">
            <thead>
              <tr className="text-zinc-500 text-xs border-b border-zinc-700">
                <th className="text-left py-2 px-3">Spec</th>
                <th className="text-center py-2 px-3">Status</th>
                <th className="text-right py-2 px-3">Staleness</th>
                <th className="text-right py-2 px-3">Files Checked</th>
              </tr>
            </thead>
            <tbody>
              {results.map(([name, data]) => (
                <tr key={name} className="border-b border-zinc-800/50">
                  <td className="py-2 px-3 text-zinc-300">{name}</td>
                  <td className="py-2 px-3 text-center">
                    {data.is_stale ? (
                      <span className="text-xs bg-red-900/30 text-red-400 px-2 py-0.5 rounded border border-red-800">
                        Stale
                      </span>
                    ) : (
                      <span className="text-xs bg-green-900/30 text-green-400 px-2 py-0.5 rounded border border-green-800">
                        Fresh
                      </span>
                    )}
                  </td>
                  <td className="py-2 px-3 text-right text-zinc-400">
                    {data.is_stale ? `${data.staleness_days}d` : "--"}
                  </td>
                  <td className="py-2 px-3 text-right text-zinc-400">
                    {data.component_files_checked}
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

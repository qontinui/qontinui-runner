/**
 * AgentCampaignHistory — browse historical per-agent campaigns.
 *
 * Lists past campaigns filtered by [per-agent: name prefix,
 * click a row to load its experiments into AgentPerformanceView.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentType } from "./PerAgentTab";
import { AgentPerformanceView } from "./AgentPerformanceView";
import { CampaignComparisonView } from "./CampaignComparisonView";

interface CampaignSummary {
  id: string;
  name: string;
  status: string;
  experiment_count: number;
  accepted_count: number;
  config_json: string;
  created_at: string;
}

interface ExperimentResult {
  config: { extra: Record<string, unknown>; multi_agent_pipeline_config?: Record<string, unknown> };
  trials: Array<{ task_run_id: string; passed: boolean; duration_ms: number }>;
  aggregate: { pass_rate: number; mean_iterations: number; mean_duration_ms: number; trial_count: number };
  accepted: boolean;
  reason: string;
  p_value?: number;
}

interface Props {
  selectedAgent: AgentType;
  onSelectCampaign: () => void;
}

interface CampaignComparison {
  campaign_a: CampaignSummary;
  campaign_b: CampaignSummary;
  pass_rate_delta: number;
  duration_delta: number;
  accepted_rate_delta: number;
}

export function AgentCampaignHistory({ selectedAgent, onSelectCampaign }: Props) {
  const [campaigns, setCampaigns] = useState<CampaignSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedCampaign, setSelectedCampaign] = useState<string | null>(null);
  const [campaignExperiments, setCampaignExperiments] = useState<
    [number, ExperimentResult][] | null
  >(null);
  const [loadingExperiments, setLoadingExperiments] = useState(false);
  const [compareMode, setCompareMode] = useState(false);
  const [compareSelection, setCompareSelection] = useState<string[]>([]);
  const [comparison, setComparison] = useState<CampaignComparison | null>(null);

  const fetchCampaigns = useCallback(async () => {
    try {
      const all = await invoke<CampaignSummary[]>(
        "get_autoresearch_campaign_history",
        { filter: `[per-agent:${selectedAgent}` },
      );
      setCampaigns(all);
    } catch {
      // DB may not be available
    } finally {
      setLoading(false);
    }
  }, [selectedAgent]);

  useEffect(() => {
    setLoading(true);
    setSelectedCampaign(null);
    setCampaignExperiments(null);
    fetchCampaigns();
  }, [fetchCampaigns]);

  async function loadCampaignExperiments(campaignId: string) {
    setSelectedCampaign(campaignId);
    setLoadingExperiments(true);
    onSelectCampaign();
    try {
      const exps = await invoke<[number, ExperimentResult][]>(
        "get_autoresearch_campaign_experiments",
        { campaignId },
      );
      setCampaignExperiments(exps);
    } catch (e) {
      console.error("Failed to load experiments:", e);
      setCampaignExperiments([]);
    } finally {
      setLoadingExperiments(false);
    }
  }

  const handleRerun = async (campaignId: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await invoke("rerun_autoresearch_campaign", { campaignId });
      fetchCampaigns();
    } catch (err) {
      console.error("Re-run failed:", err);
    }
  };

  const handleCompare = async () => {
    if (compareSelection.length !== 2) return;
    try {
      const result = await invoke<CampaignComparison>("compare_autoresearch_campaigns", {
        campaignIdA: compareSelection[0],
        campaignIdB: compareSelection[1],
      });
      setComparison(result);
    } catch (err) {
      console.error("Compare failed:", err);
    }
  };

  if (loading) {
    return <div className="text-sm text-zinc-500 p-4">Loading campaign history...</div>;
  }

  return (
    <div className="p-4 space-y-4">
      {/* Controls */}
      <div className="flex gap-2 items-center">
        <button
          onClick={() => { setCompareMode(!compareMode); setCompareSelection([]); setComparison(null); }}
          className={`text-xs px-2 py-1 rounded border ${
            compareMode
              ? "border-blue-600 bg-blue-900/30 text-blue-300"
              : "border-zinc-700 bg-zinc-800 text-zinc-400 hover:text-zinc-200"
          }`}
        >
          {compareMode ? "Cancel Compare" : "Compare"}
        </button>
        {compareMode && compareSelection.length === 2 && (
          <button
            onClick={handleCompare}
            className="text-xs px-2 py-1 rounded bg-blue-700 text-blue-100 hover:bg-blue-600"
          >
            Compare Selected
          </button>
        )}
        {compareMode && (
          <span className="text-xs text-zinc-500">{compareSelection.length}/2 selected</span>
        )}
      </div>

      {/* Comparison view */}
      {comparison && (
        <CampaignComparisonView
          comparison={comparison}
          onClose={() => { setComparison(null); setCompareSelection([]); setCompareMode(false); }}
        />
      )}

      {/* Campaign list */}
      <div className="border border-zinc-700 rounded-lg overflow-hidden">
        <table className="w-full text-xs">
          <thead className="bg-zinc-800">
            <tr>
              {compareMode && <th className="px-2 py-2 w-8"></th>}
              <th className="text-left px-3 py-2 text-zinc-400">Date</th>
              <th className="text-left px-3 py-2 text-zinc-400">Name</th>
              <th className="text-center px-3 py-2 text-zinc-400">Status</th>
              <th className="text-center px-3 py-2 text-zinc-400">Exp</th>
              <th className="text-center px-3 py-2 text-zinc-400">Accept</th>
              <th className="text-center px-3 py-2 text-zinc-400 w-16"></th>
            </tr>
          </thead>
          <tbody>
            {campaigns.length === 0 && (
              <tr>
                <td colSpan={compareMode ? 7 : 6} className="px-3 py-4 text-center text-zinc-500">
                  No per-agent campaigns found for this agent.
                </td>
              </tr>
            )}
            {campaigns.map((c) => {
              const isSelected = selectedCampaign === c.id;
              const isCompareSelected = compareSelection.includes(c.id);
              const date = c.created_at.slice(5, 10).replace("-", "/");

              return (
                <tr
                  key={c.id}
                  onClick={() => !compareMode && loadCampaignExperiments(c.id)}
                  className={`border-t border-zinc-800 cursor-pointer transition-colors ${
                    isSelected
                      ? "bg-blue-900/20"
                      : isCompareSelected
                        ? "bg-blue-900/10"
                        : "hover:bg-zinc-800/50"
                  }`}
                >
                  {compareMode && (
                    <td className="px-2 py-1.5" onClick={(e) => e.stopPropagation()}>
                      <input
                        type="checkbox"
                        checked={isCompareSelected}
                        onChange={() => {
                          setCompareSelection((prev) => {
                            if (prev.includes(c.id)) return prev.filter((id) => id !== c.id);
                            if (prev.length >= 2) return prev;
                            return [...prev, c.id];
                          });
                        }}
                        className="accent-blue-500"
                      />
                    </td>
                  )}
                  <td className="px-3 py-1.5 text-zinc-400">{date}</td>
                  <td className="px-3 py-1.5 text-zinc-200 font-mono">{c.name}</td>
                  <td className="px-3 py-1.5 text-center">
                    <span
                      className={`px-1.5 py-0.5 rounded text-[10px] ${
                        c.status === "converged"
                          ? "bg-green-900/30 text-green-400"
                          : c.status === "completed"
                            ? "bg-blue-900/30 text-blue-400"
                            : c.status === "running"
                              ? "bg-yellow-900/30 text-yellow-400"
                              : "bg-zinc-800 text-zinc-500"
                      }`}
                    >
                      {c.status}
                    </span>
                  </td>
                  <td className="px-3 py-1.5 text-center text-zinc-300">
                    {c.experiment_count}
                  </td>
                  <td className="px-3 py-1.5 text-center text-zinc-300">
                    {c.accepted_count}
                  </td>
                  <td className="px-3 py-1.5 text-center">
                    <button
                      onClick={(e) => handleRerun(c.id, e)}
                      className="text-[10px] px-1.5 py-0.5 rounded bg-zinc-700 text-zinc-300 hover:bg-zinc-600"
                    >
                      Re-run
                    </button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      {/* Selected campaign's experiments rendered in AgentPerformanceView */}
      {selectedCampaign && (
        <div className="border border-zinc-700 rounded-lg">
          <div className="px-4 py-2 border-b border-zinc-700 bg-zinc-800/30">
            <h3 className="text-xs font-medium text-zinc-300">
              Campaign Results: {campaigns.find((c) => c.id === selectedCampaign)?.name}
            </h3>
          </div>
          {loadingExperiments ? (
            <div className="text-xs text-zinc-500 p-4">Loading experiments...</div>
          ) : (
            <AgentPerformanceView
              selectedAgent={selectedAgent}
              historicalResults={campaignExperiments ?? undefined}
            />
          )}
        </div>
      )}
    </div>
  );
}

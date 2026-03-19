/**
 * CampaignTab — campaign configuration form + live monitoring via AutoresearchDashboard.
 *
 * Sends search_dimensions as typed objects matching the Rust SearchDimension enum:
 *   { type: "workflow_architecture" }
 *   { type: "model", candidates: [...] }
 *   { type: "max_iterations", range: [min, max] }
 *   { type: "context_tokens", candidates: [...] }
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { AutoresearchDashboard } from "./AutoresearchDashboard";
import type { SearchDimension } from "@/types";

const inputClass =
  "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50";
const selectClass =
  "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50";
const chipActiveClass =
  "px-2 py-0.5 text-[11px] rounded border transition-colors border-blue-500 bg-blue-900/30 text-blue-300";
const chipInactiveClass =
  "px-2 py-0.5 text-[11px] rounded border transition-colors border-zinc-700 bg-zinc-800 text-zinc-500 hover:text-zinc-300";

interface WorkflowListItem {
  id: string;
  name: string;
}

const SEARCH_DIMENSIONS = [
  { id: "workflow_architecture", label: "Workflow Architecture", alwaysOn: true },
  { id: "model", label: "Model" },
  { id: "max_iterations", label: "Max Iterations" },
  { id: "context_tokens", label: "Context Tokens" },
] as const;

const KNOWN_MODELS = [
  "claude-opus-4-20250514",
  "claude-sonnet-4-20250514",
  "claude-haiku-4-5-20251001",
  "gpt-4o",
  "gpt-4o-mini",
  "o3-mini",
];

const CONTEXT_TOKEN_OPTIONS = [4096, 8192, 16384, 32768, 65536, 131072, 200000];

export function CampaignTab() {
  const [name, setName] = useState("");
  const [workflowId, setWorkflowId] = useState("");
  const [workflows, setWorkflows] = useState<WorkflowListItem[]>([]);
  const [trialsPerExperiment, setTrialsPerExperiment] = useState(3);
  const [useWorktree, setUseWorktree] = useState(true);
  const [dimensions, setDimensions] = useState<Set<string>>(new Set(["workflow_architecture"]));
  const [mutationStrategy, setMutationStrategy] = useState("sequential");
  const [primaryMetric, setPrimaryMetric] = useState("pass_rate");
  const [significanceThreshold, setSignificanceThreshold] = useState(0.05);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Dimension-specific config
  const [modelCandidates, setModelCandidates] = useState<string[]>([
    "claude-opus-4-20250514",
    "claude-sonnet-4-20250514",
  ]);
  const [iterRange, setIterRange] = useState<[number, number]>([3, 15]);
  const [contextCandidates, setContextCandidates] = useState<number[]>([16384, 65536, 200000]);

  const fetchWorkflows = useCallback(async () => {
    try {
      const list = await invoke<WorkflowListItem[]>("list_unified_workflows");
      setWorkflows(list);
    } catch {
      // API may not be available
    }
  }, []);

  useEffect(() => {
    fetchWorkflows();
  }, [fetchWorkflows]);

  function toggleDimension(id: string) {
    setDimensions((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleModelCandidate(model: string) {
    setModelCandidates((prev) =>
      prev.includes(model) ? prev.filter((m) => m !== model) : [...prev, model],
    );
  }

  function toggleContextCandidate(tokens: number) {
    setContextCandidates((prev) =>
      prev.includes(tokens)
        ? prev.filter((t) => t !== tokens)
        : [...prev, tokens].sort((a, b) => a - b),
    );
  }

  /** Build typed SearchDimension objects matching the Rust enum. */
  function buildSearchDimensions(): SearchDimension[] {
    const result: SearchDimension[] = [];
    for (const id of dimensions) {
      switch (id) {
        case "workflow_architecture":
          result.push({ type: "workflow_architecture" });
          break;
        case "model":
          result.push({ type: "model", candidates: modelCandidates });
          break;
        case "max_iterations":
          result.push({ type: "max_iterations", range: iterRange });
          break;
        case "context_tokens":
          result.push({ type: "context_tokens", candidates: contextCandidates });
          break;
      }
    }
    return result;
  }

  async function startCampaign() {
    setError(null);
    try {
      const searchDims = buildSearchDimensions();
      if (searchDims.length === 0) {
        setError("Select at least one search dimension.");
        return;
      }

      const configJson = JSON.stringify({
        name: name || "Untitled Campaign",
        benchmark_workflow_id: workflowId,
        trials_per_experiment: trialsPerExperiment,
        control_config: {},
        search_dimensions: searchDims,
        mutation_strategy: mutationStrategy,
        acceptance_criteria: {
          primary_metric: primaryMetric,
          significance_threshold: significanceThreshold,
        },
        use_worktree: useWorktree,
      });
      await invoke("start_autoresearch", { configJson });
      setRunning(true);
    } catch (e) {
      setError(String(e));
    }
  }

  async function stopCampaign() {
    try {
      await invoke("stop_autoresearch");
      setRunning(false);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="p-4 space-y-6">
      {/* Campaign Config Form */}
      <div className="border border-zinc-700 rounded-lg p-4 space-y-4">
        <h3 className="text-sm font-bold text-zinc-200">Campaign Configuration</h3>

        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Campaign Name</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g. Architecture comparison Q1"
              className={inputClass}
            />
          </div>
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Benchmark Workflow</label>
            <select
              value={workflowId}
              onChange={(e) => setWorkflowId(e.target.value)}
              className={selectClass}
            >
              <option value="">Select workflow...</option>
              {workflows.map((w) => (
                <option key={w.id} value={w.id}>
                  {w.name}
                </option>
              ))}
            </select>
          </div>
        </div>

        <div className="grid grid-cols-3 gap-4">
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Trials per Experiment</label>
            <input
              type="number"
              min={1}
              max={20}
              value={trialsPerExperiment}
              onChange={(e) => setTrialsPerExperiment(Number(e.target.value))}
              className={inputClass}
            />
          </div>
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Mutation Strategy</label>
            <select
              value={mutationStrategy}
              onChange={(e) => setMutationStrategy(e.target.value)}
              className={selectClass}
            >
              <option value="sequential">Sequential</option>
              <option value="random_perturbation">Random Perturbation</option>
              <option value="ai_guided">AI Guided</option>
            </select>
          </div>
          <div className="flex items-end gap-2 pb-1">
            <input
              type="checkbox"
              id="use_worktree_campaign"
              checked={useWorktree}
              onChange={(e) => setUseWorktree(e.target.checked)}
              className="rounded border-zinc-600 bg-zinc-800 text-blue-500 focus:ring-blue-500/50"
            />
            <label htmlFor="use_worktree_campaign" className="text-xs text-zinc-400">
              Use worktree isolation
            </label>
          </div>
        </div>

        {/* Search Dimensions */}
        <div className="space-y-3">
          <label className="block text-xs text-zinc-500 mb-2">Search Dimensions</label>
          <div className="flex gap-4">
            {SEARCH_DIMENSIONS.map((d) => (
              <label key={d.id} className="flex items-center gap-1.5 text-xs text-zinc-400">
                <input
                  type="checkbox"
                  checked={dimensions.has(d.id)}
                  onChange={() => !("alwaysOn" in d && d.alwaysOn) && toggleDimension(d.id)}
                  disabled={"alwaysOn" in d && d.alwaysOn}
                  className="rounded border-zinc-600 bg-zinc-800 text-blue-500 focus:ring-blue-500/50"
                />
                {d.label}
              </label>
            ))}
          </div>

          {/* Workflow Architecture info */}
          {dimensions.has("workflow_architecture") && (
            <div className="ml-5 text-[11px] text-zinc-500">
              Compares: Traditional, Agentic Verification, Multi-Agent Pipeline
            </div>
          )}

          {/* Model candidates */}
          {dimensions.has("model") && (
            <div className="ml-5 space-y-1">
              <div className="text-[11px] text-zinc-500">Model candidates:</div>
              <div className="flex flex-wrap gap-1.5">
                {KNOWN_MODELS.map((m) => (
                  <button
                    key={m}
                    onClick={() => toggleModelCandidate(m)}
                    className={modelCandidates.includes(m) ? chipActiveClass : chipInactiveClass}
                  >
                    {m.replace(/^claude-/, "").replace(/-2025\d+/, "")}
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Max iterations range */}
          {dimensions.has("max_iterations") && (
            <div className="ml-5 flex items-center gap-2">
              <span className="text-[11px] text-zinc-500">Range:</span>
              <input
                type="number"
                min={1}
                max={100}
                value={iterRange[0]}
                onChange={(e) => setIterRange([Number(e.target.value), iterRange[1]])}
                className="w-16 px-2 py-1 text-[11px] bg-zinc-800 border border-zinc-700 rounded text-zinc-200"
              />
              <span className="text-[11px] text-zinc-500">to</span>
              <input
                type="number"
                min={1}
                max={100}
                value={iterRange[1]}
                onChange={(e) => setIterRange([iterRange[0], Number(e.target.value)])}
                className="w-16 px-2 py-1 text-[11px] bg-zinc-800 border border-zinc-700 rounded text-zinc-200"
              />
            </div>
          )}

          {/* Context token candidates */}
          {dimensions.has("context_tokens") && (
            <div className="ml-5 space-y-1">
              <div className="text-[11px] text-zinc-500">Token budget candidates:</div>
              <div className="flex flex-wrap gap-1.5">
                {CONTEXT_TOKEN_OPTIONS.map((t) => (
                  <button
                    key={t}
                    onClick={() => toggleContextCandidate(t)}
                    className={contextCandidates.includes(t) ? chipActiveClass : chipInactiveClass}
                  >
                    {t >= 1000 ? `${(t / 1000).toFixed(0)}k` : t}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>

        {/* Acceptance Criteria */}
        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Primary Metric</label>
            <select
              value={primaryMetric}
              onChange={(e) => setPrimaryMetric(e.target.value)}
              className={selectClass}
            >
              <option value="pass_rate">Pass Rate</option>
              <option value="mean_iterations">Mean Iterations</option>
              <option value="mean_duration">Mean Duration</option>
            </select>
          </div>
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Significance Threshold</label>
            <input
              type="number"
              step={0.01}
              min={0.001}
              max={0.5}
              value={significanceThreshold}
              onChange={(e) => setSignificanceThreshold(Number(e.target.value))}
              className={inputClass}
            />
          </div>
        </div>

        {/* Actions */}
        <div className="flex gap-2 pt-2">
          <button
            onClick={startCampaign}
            disabled={running || !workflowId}
            className="px-4 py-2 bg-blue-600 hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed text-white text-sm rounded-md transition-colors"
          >
            Start Campaign
          </button>
          <button
            onClick={stopCampaign}
            disabled={!running}
            className="px-4 py-2 bg-zinc-700 hover:bg-zinc-600 disabled:opacity-50 disabled:cursor-not-allowed text-zinc-200 text-sm rounded-md transition-colors"
          >
            Stop Campaign
          </button>
        </div>

        {error && (
          <div className="text-xs bg-red-900/30 text-red-400 rounded px-2 py-1">{error}</div>
        )}
      </div>

      {/* Live Monitor */}
      <div className="border border-zinc-700 rounded-lg">
        <div className="px-4 py-2 border-b border-zinc-700">
          <h3 className="text-sm font-bold text-zinc-200">Live Monitor</h3>
        </div>
        <AutoresearchDashboard />
      </div>
    </div>
  );
}

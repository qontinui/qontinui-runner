/**
 * AgentCampaignConfig — per-agent campaign configuration form.
 *
 * Allows configuring search dimensions for a single agent while holding
 * the other 3 agents' configs constant. Launches via existing start_autoresearch.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentType } from "./types";
import type { PipelineAgentConfig } from "@/types";

interface WorkflowListItem {
  id: string;
  name: string;
}

const inputClass =
  "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-blue-500/50 text-sm";
const selectClass =
  "w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-none focus:ring-2 focus:ring-blue-500/50 text-sm";

const KNOWN_MODELS = [
  "claude-opus-4-20250514",
  "claude-sonnet-4-20250514",
  "claude-haiku-4-5-20251001",
  "gpt-4o",
  "gpt-4o-mini",
  "o3-mini",
];

const TEMP_CANDIDATES = [0.0, 0.1, 0.3, 0.5, 0.7, 1.0];
const MAX_TOKENS_CANDIDATES = [1024, 2048, 4096, 8192, 16384];

const AGENT_LABELS: Record<AgentType, string> = {
  spec_analyst: "Spec Analyst",
  locator: "Locator",
  implementer: "Implementer",
  verifier: "Verifier",
};

const ALL_AGENTS: AgentType[] = ["spec_analyst", "locator", "implementer", "verifier"];

interface Props {
  selectedAgent: AgentType;
  onStarted: () => void;
}

export function AgentCampaignConfig({ selectedAgent, onStarted }: Props) {
  const [name, setName] = useState("");
  const [workflowId, setWorkflowId] = useState("");
  const [workflows, setWorkflows] = useState<WorkflowListItem[]>([]);
  const [trialsPerExperiment, setTrialsPerExperiment] = useState(3);
  const [mutationStrategy, setMutationStrategy] = useState("sequential");
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  // Search dimension toggles
  const [searchModel, setSearchModel] = useState(true);
  const [searchTemp, setSearchTemp] = useState(false);
  const [searchMaxTokens, setSearchMaxTokens] = useState(false);
  const [searchPromptVariant, setSearchPromptVariant] = useState(false);

  // Candidate lists
  const [modelCandidates, setModelCandidates] = useState<string[]>([
    "claude-opus-4-20250514",
    "claude-sonnet-4-20250514",
  ]);
  const [tempCandidates, setTempCandidates] = useState<number[]>([0.0, 0.3, 0.7]);
  const [maxTokensCandidates, setMaxTokensCandidates] = useState<number[]>([4096, 8192]);
  const [promptVariants, setPromptVariants] = useState<string[]>([]);
  const [newPromptVariant, setNewPromptVariant] = useState("");

  // Baseline configs for the other agents
  const [baselineConfigs, _setBaselineConfigs] = useState<Record<AgentType, PipelineAgentConfig>>({
    spec_analyst: {},
    locator: {},
    implementer: {},
    verifier: {},
  });

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

  function toggleModelCandidate(model: string) {
    setModelCandidates((prev) =>
      prev.includes(model) ? prev.filter((m) => m !== model) : [...prev, model],
    );
  }

  function toggleTempCandidate(temp: number) {
    setTempCandidates((prev) =>
      prev.includes(temp) ? prev.filter((t) => t !== temp) : [...prev, temp].sort(),
    );
  }

  function toggleMaxTokensCandidate(tokens: number) {
    setMaxTokensCandidates((prev) =>
      prev.includes(tokens)
        ? prev.filter((t) => t !== tokens)
        : [...prev, tokens].sort((a, b) => a - b),
    );
  }

  function addPromptVariant() {
    const v = newPromptVariant.trim();
    if (v && !promptVariants.includes(v)) {
      setPromptVariants((prev) => [...prev, v]);
      setNewPromptVariant("");
    }
  }

  function removePromptVariant(v: string) {
    setPromptVariants((prev) => prev.filter((p) => p !== v));
  }

  async function startCampaign() {
    setError(null);
    setStarting(true);

    try {
      // Build search dimensions as Custom entries with pipeline_agent.* naming
      const searchDimensions: Array<{
        type: string;
        name: string;
        candidates: unknown[];
      }> = [];

      if (searchModel && modelCandidates.length > 0) {
        searchDimensions.push({
          type: "custom",
          name: `pipeline_agent.${selectedAgent}.model`,
          candidates: modelCandidates,
        });
      }
      if (searchTemp && tempCandidates.length > 0) {
        searchDimensions.push({
          type: "custom",
          name: `pipeline_agent.${selectedAgent}.temperature`,
          candidates: tempCandidates,
        });
      }
      if (searchMaxTokens && maxTokensCandidates.length > 0) {
        searchDimensions.push({
          type: "custom",
          name: `pipeline_agent.${selectedAgent}.max_tokens`,
          candidates: maxTokensCandidates,
        });
      }
      if (searchPromptVariant && promptVariants.length > 0) {
        searchDimensions.push({
          type: "custom",
          name: `pipeline_agent.${selectedAgent}.prompt_variant`,
          candidates: promptVariants,
        });
      }

      if (searchDimensions.length === 0) {
        setError("Select at least one search dimension with candidates.");
        setStarting(false);
        return;
      }

      // Build multi_agent_pipeline_config with all 4 agents' baseline
      const pipelineConfig: Record<string, PipelineAgentConfig> = {};
      for (const agent of ALL_AGENTS) {
        pipelineConfig[agent] = baselineConfigs[agent] ?? {};
      }

      const campaignName = name || `[per-agent:${selectedAgent}] Untitled`;
      const autoName = campaignName.startsWith("[per-agent:")
        ? campaignName
        : `[per-agent:${selectedAgent}] ${campaignName}`;

      const configJson = JSON.stringify({
        name: autoName,
        benchmark_workflow_id: workflowId,
        trials_per_experiment: trialsPerExperiment,
        control_config: {
          workflow_architecture: "multi_agent_pipeline",
          multi_agent_pipeline_config: pipelineConfig,
        },
        search_dimensions: searchDimensions,
        mutation_strategy: mutationStrategy,
        acceptance_criteria: {
          primary_metric: "pass_rate",
          significance_threshold: 0.05,
        },
        use_worktree: true,
      });

      await invoke("start_autoresearch", { configJson });
      onStarted();
    } catch (e) {
      setError(String(e));
    } finally {
      setStarting(false);
    }
  }

  const otherAgents = ALL_AGENTS.filter((a) => a !== selectedAgent);

  return (
    <div className="p-4 space-y-4">
      {/* Campaign Basics */}
      <div className="border border-zinc-700 rounded-lg p-4 space-y-3">
        <h3 className="text-sm font-bold text-zinc-200">
          Campaign — {AGENT_LABELS[selectedAgent]}
        </h3>

        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className="block text-xs text-zinc-500 mb-1">Campaign Name</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={`e.g. ${selectedAgent} model comparison`}
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

        <div className="grid grid-cols-2 gap-3">
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
        </div>
      </div>

      {/* Baseline (other agents) — read-only display */}
      <div className="border border-zinc-700 rounded-lg p-4">
        <h3 className="text-sm font-bold text-zinc-200 mb-2">Held Constant (other agents)</h3>
        <div className="grid grid-cols-3 gap-2">
          {otherAgents.map((agent) => (
            <div key={agent} className="bg-zinc-800/50 border border-zinc-700 rounded-md p-2">
              <div className="text-xs font-medium text-zinc-300 mb-1">{AGENT_LABELS[agent]}</div>
              <div className="text-[10px] text-zinc-500 space-y-0.5">
                <div>Model: {baselineConfigs[agent]?.model || "(default)"}</div>
                <div>Temp: {baselineConfigs[agent]?.temperature ?? "(default)"}</div>
                <div>Max Tokens: {baselineConfigs[agent]?.max_tokens ?? "(default)"}</div>
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* Search Dimensions for target agent */}
      <div className="border border-zinc-700 rounded-lg p-4 space-y-3">
        <h3 className="text-sm font-bold text-zinc-200">
          Search Dimensions — {AGENT_LABELS[selectedAgent]}
        </h3>

        {/* Model */}
        <div className="space-y-1">
          <label className="flex items-center gap-2 text-xs text-zinc-400">
            <input
              type="checkbox"
              checked={searchModel}
              onChange={(e) => setSearchModel(e.target.checked)}
              className="rounded border-zinc-600 bg-zinc-800 text-blue-500"
            />
            Model
          </label>
          {searchModel && (
            <div className="ml-5 flex flex-wrap gap-1.5">
              {KNOWN_MODELS.map((m) => (
                <button
                  key={m}
                  onClick={() => toggleModelCandidate(m)}
                  className={`px-2 py-0.5 text-[11px] rounded border transition-colors ${
                    modelCandidates.includes(m)
                      ? "border-blue-500 bg-blue-900/30 text-blue-300"
                      : "border-zinc-700 bg-zinc-800 text-zinc-500 hover:text-zinc-300"
                  }`}
                >
                  {m.replace(/^claude-/, "").replace(/-2025\d+/, "")}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Temperature */}
        <div className="space-y-1">
          <label className="flex items-center gap-2 text-xs text-zinc-400">
            <input
              type="checkbox"
              checked={searchTemp}
              onChange={(e) => setSearchTemp(e.target.checked)}
              className="rounded border-zinc-600 bg-zinc-800 text-blue-500"
            />
            Temperature
          </label>
          {searchTemp && (
            <div className="ml-5 flex flex-wrap gap-1.5">
              {TEMP_CANDIDATES.map((t) => (
                <button
                  key={t}
                  onClick={() => toggleTempCandidate(t)}
                  className={`px-2 py-0.5 text-[11px] rounded border transition-colors ${
                    tempCandidates.includes(t)
                      ? "border-blue-500 bg-blue-900/30 text-blue-300"
                      : "border-zinc-700 bg-zinc-800 text-zinc-500 hover:text-zinc-300"
                  }`}
                >
                  {t.toFixed(1)}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Max Tokens */}
        <div className="space-y-1">
          <label className="flex items-center gap-2 text-xs text-zinc-400">
            <input
              type="checkbox"
              checked={searchMaxTokens}
              onChange={(e) => setSearchMaxTokens(e.target.checked)}
              className="rounded border-zinc-600 bg-zinc-800 text-blue-500"
            />
            Max Tokens
          </label>
          {searchMaxTokens && (
            <div className="ml-5 flex flex-wrap gap-1.5">
              {MAX_TOKENS_CANDIDATES.map((t) => (
                <button
                  key={t}
                  onClick={() => toggleMaxTokensCandidate(t)}
                  className={`px-2 py-0.5 text-[11px] rounded border transition-colors ${
                    maxTokensCandidates.includes(t)
                      ? "border-blue-500 bg-blue-900/30 text-blue-300"
                      : "border-zinc-700 bg-zinc-800 text-zinc-500 hover:text-zinc-300"
                  }`}
                >
                  {t.toLocaleString()}
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Prompt Variant */}
        <div className="space-y-1">
          <label className="flex items-center gap-2 text-xs text-zinc-400">
            <input
              type="checkbox"
              checked={searchPromptVariant}
              onChange={(e) => setSearchPromptVariant(e.target.checked)}
              className="rounded border-zinc-600 bg-zinc-800 text-blue-500"
            />
            Prompt Variant
          </label>
          {searchPromptVariant && (
            <div className="ml-5 space-y-1">
              <div className="flex flex-wrap gap-1">
                {promptVariants.map((v) => (
                  <span
                    key={v}
                    className="inline-flex items-center gap-1 px-2 py-0.5 text-[11px] rounded border border-blue-500 bg-blue-900/30 text-blue-300"
                  >
                    {v}
                    <button
                      onClick={() => removePromptVariant(v)}
                      className="text-blue-400 hover:text-red-400"
                    >
                      x
                    </button>
                  </span>
                ))}
              </div>
              <div className="flex gap-1">
                <input
                  type="text"
                  value={newPromptVariant}
                  onChange={(e) => setNewPromptVariant(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && addPromptVariant()}
                  placeholder="Add variant name..."
                  className="px-2 py-1 text-[11px] bg-zinc-800 border border-zinc-700 rounded text-zinc-200 placeholder-zinc-600"
                />
                <button
                  onClick={addPromptVariant}
                  className="px-2 py-1 text-[11px] bg-zinc-700 text-zinc-300 rounded hover:bg-zinc-600"
                >
                  Add
                </button>
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Actions */}
      <div className="flex gap-2">
        <button
          onClick={startCampaign}
          disabled={starting || !workflowId}
          className="px-4 py-2 bg-blue-600 hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed text-white text-sm rounded-md transition-colors"
        >
          {starting ? "Starting..." : "Start Per-Agent Campaign"}
        </button>
      </div>

      {error && <div className="text-xs bg-red-900/30 text-red-400 rounded px-2 py-1">{error}</div>}
    </div>
  );
}

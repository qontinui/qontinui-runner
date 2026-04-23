/**
 * PromptRegistryTab — Prompt variants per agent with active indicator and performance metrics.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { PromptVersionHistory } from "../prompt-versions";
import { PromptCanaryPanel } from "./PromptCanaryPanel";
import { CreatePromptCanaryForm } from "./CreatePromptCanaryForm";

interface PromptVariant {
  id: string;
  agent_type: string;
  variant_name: string;
  prompt_content: string;
  version: number;
  is_active: boolean;
  source_recommendation_id: string | null;
  performance_metrics: string | null;
  created_at: string;
  updated_at: string;
}

const AGENT_TYPES = ["spec_analyst", "locator", "implementer", "verifier"] as const;

export function PromptRegistryTab() {
  const [variants, setVariants] = useState<PromptVariant[]>([]);
  const [filterAgent, setFilterAgent] = useState<string>("all");
  const [loading, setLoading] = useState(true);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [versionHistoryId, setVersionHistoryId] = useState<string | null>(null);
  const [canaryFormVariantId, setCanaryFormVariantId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<PromptVariant[]>("get_prompt_variants", {
        agentType: filterAgent === "all" ? null : filterAgent,
      });
      setVariants(result);
    } catch (e) {
      console.error("Failed to load prompt variants:", e);
    } finally {
      setLoading(false);
    }
  }, [filterAgent]);

  useEffect(() => {
    // Async IIFE: avoid invoking a setState-bearing callback synchronously
    // in the effect body (set-state-in-effect).
    let cancelled = false;
    void (async () => {
      if (cancelled) return;
      await load();
    })();
    return () => {
      cancelled = true;
    };
  }, [load]);

  const handleActivate = async (variantId: string) => {
    try {
      await invoke("activate_prompt_variant", { variantId });
      load();
    } catch (e) {
      console.error("Failed to activate variant:", e);
    }
  };

  const grouped = AGENT_TYPES.reduce(
    (acc, agentType) => {
      acc[agentType] = variants.filter((v) => v.agent_type === agentType);
      return acc;
    },
    {} as Record<string, PromptVariant[]>,
  );

  return (
    <div className="p-4 space-y-4" data-tutorial-id="prompt-registry">
      <PromptCanaryPanel />

      <div className="border-t border-zinc-800 pt-4" />

      <div className="flex items-center gap-3">
        <select
          className="bg-zinc-800 text-zinc-200 text-sm px-2 py-1 rounded border border-zinc-700"
          value={filterAgent}
          onChange={(e) => setFilterAgent(e.target.value)}
        >
          <option value="all">All Agents</option>
          {AGENT_TYPES.map((at) => (
            <option key={at} value={at}>
              {at}
            </option>
          ))}
        </select>
        <button onClick={load} className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1">
          Refresh
        </button>
        <span className="text-xs text-zinc-500 ml-auto">{variants.length} variants</span>
      </div>

      {loading ? (
        <div className="text-zinc-500 text-sm">Loading...</div>
      ) : variants.length === 0 ? (
        <div className="text-zinc-500 text-sm py-8 text-center">
          No prompt variants in the registry. The Pipeline Prompt Optimizer will create them.
        </div>
      ) : (
        <div className="space-y-6">
          {AGENT_TYPES.filter((at) => filterAgent === "all" || filterAgent === at).map(
            (agentType) => {
              const agentVariants = grouped[agentType];
              if (!agentVariants || agentVariants.length === 0) return null;
              const active = agentVariants.find((v) => v.is_active);

              return (
                <div key={agentType} className="space-y-2">
                  <div className="flex items-center gap-2">
                    <h3 className="text-sm font-medium text-zinc-200">{agentType}</h3>
                    {active && (
                      <span className="text-xs text-green-400">
                        Active: {active.variant_name} v{active.version}
                      </span>
                    )}
                    {!active && <span className="text-xs text-zinc-500">Using default prompt</span>}
                  </div>

                  {agentVariants.map((v) => (
                    <div
                      key={v.id}
                      className={`bg-zinc-900 border rounded-lg overflow-hidden ${
                        v.is_active ? "border-green-800" : "border-zinc-800"
                      }`}
                    >
                      <div
                        className="flex items-center gap-3 px-4 py-2 cursor-pointer hover:bg-zinc-800/50"
                        onClick={() => setExpanded(expanded === v.id ? null : v.id)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            setExpanded(expanded === v.id ? null : v.id);
                          }
                        }}
                        role="button"
                        tabIndex={0}
                      >
                        {v.is_active && (
                          <span className="text-xs px-2 py-0.5 rounded bg-green-900/30 text-green-400">
                            active
                          </span>
                        )}
                        <span className="text-sm text-zinc-200">
                          {v.variant_name} <span className="text-zinc-500">v{v.version}</span>
                        </span>
                        <span className="text-xs text-zinc-600 ml-auto">
                          {new Date(v.created_at).toLocaleDateString()}
                        </span>
                      </div>

                      {expanded === v.id && (
                        <div className="px-4 pb-4 space-y-3 border-t border-zinc-800">
                          <pre className="text-xs text-zinc-300 bg-zinc-800/50 p-3 rounded overflow-auto max-h-64 mt-3 whitespace-pre-wrap">
                            {v.prompt_content}
                          </pre>

                          {v.performance_metrics && v.performance_metrics !== "{}" && (
                            <div>
                              <div className="text-xs text-zinc-500 mb-1">Performance Metrics</div>
                              <pre className="text-xs text-zinc-400 bg-zinc-800/30 p-2 rounded">
                                {v.performance_metrics}
                              </pre>
                            </div>
                          )}

                          <div className="flex items-center gap-2">
                            {!v.is_active && (
                              <button
                                onClick={() => handleActivate(v.id)}
                                className="px-3 py-1 text-xs bg-green-800 text-green-200 rounded hover:bg-green-700"
                              >
                                Activate
                              </button>
                            )}
                            <button
                              onClick={() =>
                                setVersionHistoryId(versionHistoryId === v.id ? null : v.id)
                              }
                              className={`px-3 py-1 text-xs rounded ${
                                versionHistoryId === v.id
                                  ? "bg-blue-800 text-blue-200"
                                  : "bg-zinc-700 text-zinc-300 hover:bg-zinc-600"
                              }`}
                            >
                              Version History
                            </button>
                            <button
                              onClick={() =>
                                setCanaryFormVariantId(canaryFormVariantId === v.id ? null : v.id)
                              }
                              className={`px-3 py-1 text-xs rounded ${
                                canaryFormVariantId === v.id
                                  ? "bg-purple-800 text-purple-200"
                                  : "bg-zinc-700 text-zinc-300 hover:bg-zinc-600"
                              }`}
                            >
                              Create A/B Test
                            </button>
                          </div>

                          {versionHistoryId === v.id && (
                            <div className="border border-zinc-700 rounded-lg overflow-hidden mt-2">
                              <PromptVersionHistory templateId={v.id} />
                            </div>
                          )}

                          {canaryFormVariantId === v.id && (
                            <div className="mt-2">
                              <CreatePromptCanaryForm
                                defaultTemplateId={v.id}
                                defaultBaselineVersion={v.version}
                                onCreated={() => setCanaryFormVariantId(null)}
                                onClose={() => setCanaryFormVariantId(null)}
                              />
                            </div>
                          )}
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              );
            },
          )}
        </div>
      )}
    </div>
  );
}

/**
 * EvalSpecsTab — CRUD management for eval specs (promptfoo-inspired evaluation).
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface EvalAssertion {
  type: string;
  min?: number;
  ms?: number;
  cents?: number;
  count?: number;
  metric?: string;
  tolerance_pp?: number;
}

interface EvalTestCase {
  id: string;
  description: string;
  input: { type: string; workflow_id?: string; ids?: string[] };
  assertions: EvalAssertion[];
}

interface EvalThresholds {
  min_trials: number;
  significance_threshold: number;
  required_pass_rate: number;
}

interface EvalSpec {
  id: string;
  name: string;
  target_agent: string | null;
  test_cases: EvalTestCase[];
  thresholds: EvalThresholds;
}

export function EvalSpecsTab() {
  const [specs, setSpecs] = useState<EvalSpec[]>([]);
  const [loading, setLoading] = useState(true);
  const [targetAgent, setTargetAgent] = useState<string>("all");
  const [expanded, setExpanded] = useState<string | null>(null);
  const [generating, setGenerating] = useState(false);

  const agentOptions = Array.from(
    new Set(specs.map((s) => s.target_agent).filter(Boolean) as string[]),
  );

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<EvalSpec[]>("get_eval_specs", {
        targetAgent: targetAgent === "all" ? null : targetAgent,
      });
      setSpecs(result);
    } catch (e) {
      console.error("Failed to load eval specs:", e);
    } finally {
      setLoading(false);
    }
  }, [targetAgent]);

  useEffect(() => {
    load();
  }, [load]);

  const handleGenerateDefault = useCallback(async () => {
    if (!targetAgent || targetAgent === "all") return;
    try {
      setGenerating(true);
      await invoke<EvalSpec>("generate_default_eval_spec", { targetAgent });
      await load();
    } catch (e) {
      console.error("Failed to generate default eval spec:", e);
    } finally {
      setGenerating(false);
    }
  }, [targetAgent, load]);

  const handleDelete = useCallback(
    async (specId: string) => {
      try {
        await invoke("delete_eval_spec", { specId });
        await load();
      } catch (e) {
        console.error("Failed to delete eval spec:", e);
      }
    },
    [load],
  );

  const formatAssertionLabel = (a: EvalAssertion): string => {
    switch (a.type) {
      case "min_success_rate":
        return `Success Rate >= ${((a.min ?? 0) * 100).toFixed(0)}%`;
      case "max_latency":
        return `Latency <= ${a.ms ?? 0}ms`;
      case "max_cost":
        return `Cost <= ${a.cents ?? 0}c`;
      case "min_sample_size":
        return `Sample Size >= ${a.count ?? 0}`;
      case "metric_regression":
        return `${a.metric ?? "metric"} regression <= ${a.tolerance_pp ?? 0}pp`;
      default:
        return a.type;
    }
  };

  return (
    <div className="p-4 space-y-4">
      {/* Controls */}
      <div className="flex gap-3 items-center">
        <select
          className="bg-zinc-800 text-zinc-200 text-sm px-2 py-1 rounded border border-zinc-700"
          value={targetAgent}
          onChange={(e) => setTargetAgent(e.target.value)}
        >
          <option value="all">All Agents</option>
          {agentOptions.map((a) => (
            <option key={a} value={a}>
              {a}
            </option>
          ))}
        </select>
        <button
          onClick={handleGenerateDefault}
          disabled={targetAgent === "all" || generating}
          className="px-3 py-1 text-xs bg-blue-800 text-blue-200 rounded hover:bg-blue-700 disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {generating ? "Generating..." : "Generate Default"}
        </button>
        <button onClick={load} className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1">
          Refresh
        </button>
        <span className="text-xs text-zinc-500 ml-auto">{specs.length} specs</span>
      </div>

      {loading ? (
        <div className="text-zinc-500 text-sm">Loading...</div>
      ) : specs.length === 0 ? (
        <div className="text-zinc-500 text-sm py-8 text-center">
          No eval specs found. Select a target agent and click "Generate Default" to create one.
        </div>
      ) : (
        <div className="space-y-2">
          {specs.map((spec) => (
            <div
              key={spec.id}
              className="bg-zinc-900 border border-zinc-800 rounded-lg overflow-hidden"
            >
              <div
                className="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-zinc-800/50"
                onClick={() => setExpanded(expanded === spec.id ? null : spec.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    setExpanded(expanded === spec.id ? null : spec.id);
                  }
                }}
                role="button"
                tabIndex={0}
              >
                <span className="text-sm text-zinc-200 flex-1">{spec.name}</span>
                {spec.target_agent && (
                  <span className="text-xs text-zinc-500">{spec.target_agent}</span>
                )}
                <span className="text-xs text-zinc-500">
                  {spec.test_cases.length} test case{spec.test_cases.length !== 1 ? "s" : ""}
                </span>
                <span className="text-xs text-zinc-600">
                  min {spec.thresholds.min_trials} trials | sig{" "}
                  {spec.thresholds.significance_threshold} | pass{" "}
                  {(spec.thresholds.required_pass_rate * 100).toFixed(0)}%
                </span>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    handleDelete(spec.id);
                  }}
                  className="px-3 py-1 text-xs bg-red-800 text-red-200 rounded hover:bg-red-700"
                >
                  Delete
                </button>
              </div>

              {expanded === spec.id && (
                <div className="px-4 pb-4 space-y-3 border-t border-zinc-800">
                  {/* Thresholds */}
                  <div className="mt-3 flex gap-4 text-xs">
                    <div>
                      <span className="text-zinc-500">Min Trials: </span>
                      <span className="text-zinc-300">{spec.thresholds.min_trials}</span>
                    </div>
                    <div>
                      <span className="text-zinc-500">Significance: </span>
                      <span className="text-zinc-300">
                        {spec.thresholds.significance_threshold}
                      </span>
                    </div>
                    <div>
                      <span className="text-zinc-500">Required Pass Rate: </span>
                      <span className="text-zinc-300">
                        {(spec.thresholds.required_pass_rate * 100).toFixed(0)}%
                      </span>
                    </div>
                  </div>

                  {/* Test cases */}
                  <div className="space-y-2">
                    <div className="text-xs text-zinc-500">Test Cases</div>
                    {spec.test_cases.map((tc) => (
                      <div key={tc.id} className="bg-zinc-800/50 rounded p-3 space-y-2">
                        <div className="flex items-center gap-2">
                          <span className="text-sm text-zinc-200">{tc.description}</span>
                          <span className="text-xs text-zinc-600">
                            input: {tc.input.type}
                            {tc.input.workflow_id ? ` (${tc.input.workflow_id})` : ""}
                          </span>
                        </div>
                        {tc.assertions.length > 0 && (
                          <div className="flex flex-wrap gap-2">
                            {tc.assertions.map((a, i) => (
                              <span
                                key={i}
                                className="text-xs px-2 py-0.5 rounded bg-zinc-700 text-zinc-300"
                              >
                                {formatAssertionLabel(a)}
                              </span>
                            ))}
                          </div>
                        )}
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

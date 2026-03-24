/**
 * RecommendationsTab — Table of meta-optimizer recommendations with Apply/Reject/Rollback.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface RobustnessReport {
  passed: number;
  failed: number;
  total: number;
}

interface RecommendationOutcome {
  verdict: string;
  success_rate_delta?: number;
  duration_delta_ms?: number;
  cost_delta_usd?: number;
  metrics_before?: Record<string, number>;
  metrics_after?: Record<string, number>;
  p_value?: number | null;
  confidence_interval?: [number, number] | null;
  effect_size?: number | null;
}

interface EvalResult {
  id: string;
  spec_id: string;
  recommendation_id: string | null;
  status: string;
  test_case_results: {
    test_case_id: string;
    passed: boolean;
    assertion_results: {
      assertion_type: string;
      passed: boolean;
      actual_value: number;
      threshold_value: number;
      message: string;
    }[];
  }[];
  baseline_metrics: {
    success_rate: number;
    mean_duration_ms: number;
    mean_iterations: number;
    mean_cost_cents: number;
    trial_count: number;
  } | null;
  candidate_metrics: {
    success_rate: number;
    mean_duration_ms: number;
    mean_iterations: number;
    mean_cost_cents: number;
    trial_count: number;
  } | null;
  comparison: {
    success_rate_delta_pp: number;
    p_value: number | null;
    confidence_interval: [number, number] | null;
    effect_size: number | null;
    verdict: string;
  } | null;
  trials_run: number;
  created_at: string;
}

interface CascadeEffect {
  affected_agent: string;
  before_success_rate: number;
  after_success_rate: number;
  delta: number;
  sample_size_before: number;
  sample_size_after: number;
}

interface Recommendation {
  id: string;
  optimizer_type: string;
  recommendation_type: string;
  target_agent: string | null;
  title: string;
  description: string;
  current_value: string | null;
  recommended_value: string | null;
  evidence: string | null;
  confidence: number;
  status: string;
  applied_at: string | null;
  outcome_after_apply: string | null;
  optimizer_run_id: string | null;
  created_at: string;
  eval_result_id: string | null;
  eval_status: string | null;
}

const EVAL_STATUS_COLORS: Record<string, string> = {
  untested: "text-zinc-400 bg-zinc-800/50",
  passed: "text-green-400 bg-green-900/30",
  failed: "text-red-400 bg-red-900/30",
  inconclusive: "text-yellow-400 bg-yellow-900/30",
};

const STATUS_COLORS: Record<string, string> = {
  pending: "text-yellow-400 bg-yellow-900/30",
  applied: "text-green-400 bg-green-900/30",
  rejected: "text-zinc-500 bg-zinc-800/50",
  rolled_back: "text-orange-400 bg-orange-900/30",
  superseded: "text-zinc-600 bg-zinc-800/30",
  canary: "text-purple-400 bg-purple-900/30",
};

const VERDICT_BADGES: Record<string, { label: string; className: string }> = {
  improved: { label: "Improved", className: "text-green-400 bg-green-900/30" },
  regressed: { label: "Regressed", className: "text-red-400 bg-red-900/30" },
  neutral: { label: "Neutral", className: "text-zinc-400 bg-zinc-800/50" },
  insufficient_data: { label: "Pending", className: "text-yellow-400 bg-yellow-900/30" },
};

const TYPE_LABELS: Record<string, string> = {
  pipeline_prompt: "Prompt",
  architecture: "Architecture",
  generation_template: "Generation",
};

export function RecommendationsTab() {
  const [recs, setRecs] = useState<Recommendation[]>([]);
  const [filterType, setFilterType] = useState<string>("all");
  const [filterStatus, setFilterStatus] = useState<string>("all");
  const [loading, setLoading] = useState(true);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [cascadeEffects, setCascadeEffects] = useState<Record<string, CascadeEffect[]>>({});
  const [evalResults, setEvalResults] = useState<Record<string, EvalResult[]>>({});
  const [runningEval, setRunningEval] = useState<string | null>(null);
  const [robustnessStatus, setRobustnessStatus] = useState<Record<string, RobustnessReport>>({});
  const [runningRobustness, setRunningRobustness] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<Recommendation[]>("get_meta_optimizer_recommendations", {
        optimizerType: filterType === "all" ? null : filterType,
        status: filterStatus === "all" ? null : filterStatus,
      });
      setRecs(result);
    } catch (e) {
      console.error("Failed to load recommendations:", e);
    } finally {
      setLoading(false);
    }
  }, [filterType, filterStatus]);

  useEffect(() => {
    load();
  }, [load]);

  const handleAction = async (id: string, action: "apply" | "reject" | "rollback" | "canary") => {
    try {
      if (action === "canary") {
        await invoke("start_canary_rollout", { recommendationId: id, percentage: 10 });
      } else {
        const cmd =
          action === "apply"
            ? "apply_meta_optimizer_recommendation"
            : action === "reject"
              ? "reject_meta_optimizer_recommendation"
              : "rollback_meta_optimizer_recommendation";
        await invoke(cmd, { recommendationId: id });
      }
      load();
    } catch (e) {
      console.error(`Failed to ${action} recommendation:`, e);
    }
  };

  const handleReevaluate = async (id: string) => {
    try {
      await invoke("reevaluate_recommendation_outcome", { recommendationId: id });
      load();
    } catch (e) {
      console.error("Re-evaluate failed:", e);
    }
  };

  const loadCascade = async (recId: string) => {
    if (cascadeEffects[recId]) return;
    try {
      const effects = await invoke<CascadeEffect[]>("get_agent_cascade_effect", {
        recommendationId: recId,
      });
      setCascadeEffects((prev) => ({ ...prev, [recId]: effects }));
    } catch {
      // silently fail
    }
  };

  const handleRunEval = async (recommendationId: string) => {
    try {
      setRunningEval(recommendationId);
      await invoke<EvalResult>("run_recommendation_eval", { recommendationId });
      await load();
    } catch (e) {
      console.error("Failed to run eval:", e);
    } finally {
      setRunningEval(null);
    }
  };

  const loadEvalResults = async (recId: string) => {
    if (evalResults[recId]) return;
    try {
      const results = await invoke<EvalResult[]>("get_eval_results", { recommendationId: recId });
      setEvalResults((prev) => ({ ...prev, [recId]: results }));
    } catch {
      // silently fail
    }
  };

  const handleRunRobustness = async (rec: Recommendation) => {
    try {
      setRunningRobustness(rec.id);
      const report = await invoke<RobustnessReport>("run_robustness_test", {
        agentType: rec.target_agent,
        recommendationId: rec.id,
      });
      setRobustnessStatus((prev) => ({ ...prev, [rec.id]: report }));
    } catch (e) {
      console.error("Failed to run robustness test:", e);
    } finally {
      setRunningRobustness(null);
    }
  };

  return (
    <div className="p-4 space-y-4">
      {/* Filters */}
      <div className="flex gap-3 items-center">
        <select
          className="bg-zinc-800 text-zinc-200 text-sm px-2 py-1 rounded border border-zinc-700"
          value={filterType}
          onChange={(e) => setFilterType(e.target.value)}
        >
          <option value="all">All Types</option>
          <option value="pipeline_prompt">Prompt</option>
          <option value="architecture">Architecture</option>
          <option value="generation_template">Generation</option>
        </select>
        <select
          className="bg-zinc-800 text-zinc-200 text-sm px-2 py-1 rounded border border-zinc-700"
          value={filterStatus}
          onChange={(e) => setFilterStatus(e.target.value)}
        >
          <option value="all">All Statuses</option>
          <option value="pending">Pending</option>
          <option value="applied">Applied</option>
          <option value="canary">Canary</option>
          <option value="rejected">Rejected</option>
          <option value="rolled_back">Rolled Back</option>
        </select>
        <button onClick={load} className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1">
          Refresh
        </button>
        <span className="text-xs text-zinc-500 ml-auto">{recs.length} recommendations</span>
      </div>

      {loading ? (
        <div className="text-zinc-500 text-sm">Loading...</div>
      ) : recs.length === 0 ? (
        <div className="text-zinc-500 text-sm py-8 text-center">
          No recommendations yet. Run workflows to accumulate data, then trigger the meta-optimizer.
        </div>
      ) : (
        <div className="space-y-2">
          {recs.map((rec) => (
            <div
              key={rec.id}
              className="bg-zinc-900 border border-zinc-800 rounded-lg overflow-hidden"
            >
              <div
                className="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-zinc-800/50"
                onClick={() => {
                  const newExpanded = expanded === rec.id ? null : rec.id;
                  setExpanded(newExpanded);
                  if (newExpanded) {
                    if (rec.status === "applied" && rec.target_agent) {
                      loadCascade(rec.id);
                    }
                    if (rec.eval_result_id) {
                      loadEvalResults(rec.id);
                    }
                  }
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    const newExpanded = expanded === rec.id ? null : rec.id;
                    setExpanded(newExpanded);
                    if (newExpanded) {
                      if (rec.status === "applied" && rec.target_agent) {
                        loadCascade(rec.id);
                      }
                      if (rec.eval_result_id) {
                        loadEvalResults(rec.id);
                      }
                    }
                  }
                }}
                role="button"
                tabIndex={0}
              >
                <span className={`text-xs px-2 py-0.5 rounded ${STATUS_COLORS[rec.status] || ""}`}>
                  {rec.status}
                </span>
                {rec.eval_status && (
                  <span
                    className={`text-xs px-1.5 py-0.5 rounded ${EVAL_STATUS_COLORS[rec.eval_status] || EVAL_STATUS_COLORS.untested}`}
                  >
                    eval: {rec.eval_status}
                  </span>
                )}
                {rec.recommendation_type === "prompt_rewrite" &&
                  (robustnessStatus[rec.id] ? (
                    <span className="text-xs text-zinc-400 ml-1">
                      [robustness: {robustnessStatus[rec.id].passed}/
                      {robustnessStatus[rec.id].total} passed]
                    </span>
                  ) : (
                    <span className="text-xs text-zinc-500 ml-1">[robustness: untested]</span>
                  ))}
                {rec.status === "applied" &&
                  rec.outcome_after_apply &&
                  (() => {
                    try {
                      const outcome: RecommendationOutcome = JSON.parse(rec.outcome_after_apply);
                      const badge = VERDICT_BADGES[outcome.verdict];
                      if (badge) {
                        return (
                          <span className={`text-xs px-1.5 py-0.5 rounded ${badge.className}`}>
                            {badge.label}
                          </span>
                        );
                      }
                    } catch {
                      /* ignore */
                    }
                    return null;
                  })()}
                <span className="text-xs text-zinc-500">
                  {TYPE_LABELS[rec.optimizer_type] || rec.optimizer_type}
                </span>
                {rec.target_agent && (
                  <span className="text-xs text-zinc-600">{rec.target_agent}</span>
                )}
                <span className="text-sm text-zinc-200 flex-1">{rec.title}</span>
                <span className="text-xs text-zinc-500">
                  {(rec.confidence * 100).toFixed(0)}% conf
                </span>
                <span className="text-xs text-zinc-600">
                  {new Date(rec.created_at).toLocaleDateString()}
                </span>
              </div>

              {expanded === rec.id && (
                <div className="px-4 pb-4 space-y-3 border-t border-zinc-800">
                  <p className="text-sm text-zinc-300 mt-3">{rec.description}</p>

                  {rec.recommended_value && (
                    <div className="space-y-1">
                      <div className="text-xs text-zinc-500">Recommended Value</div>
                      <pre className="text-xs text-zinc-300 bg-zinc-800/50 p-2 rounded overflow-auto max-h-48">
                        {rec.recommended_value}
                      </pre>
                    </div>
                  )}

                  {rec.evidence && (
                    <div className="space-y-1">
                      <div className="text-xs text-zinc-500">Evidence</div>
                      <pre className="text-xs text-zinc-400 bg-zinc-800/30 p-2 rounded overflow-auto max-h-32">
                        {rec.evidence}
                      </pre>
                    </div>
                  )}

                  {/* Model profile link for architecture/config recs */}
                  {(rec.optimizer_type === "architecture" ||
                    rec.recommendation_type === "config_change") && (
                    <p className="text-xs text-zinc-500 italic">
                      Check Model Profiles in Autoresearch for cost-efficiency data.
                    </p>
                  )}

                  {/* Outcome details for applied recs */}
                  {rec.status === "applied" &&
                    rec.outcome_after_apply &&
                    (() => {
                      try {
                        const outcome: RecommendationOutcome = JSON.parse(rec.outcome_after_apply);
                        return (
                          <div className="bg-zinc-800/50 rounded p-3 space-y-2">
                            <div className="text-xs text-zinc-500">Outcome After Apply</div>
                            <div className="flex gap-4 text-xs">
                              {outcome.success_rate_delta != null && (
                                <div>
                                  <span className="text-zinc-500">Success Rate: </span>
                                  <span
                                    className={
                                      outcome.success_rate_delta > 0
                                        ? "text-green-400"
                                        : outcome.success_rate_delta < -2
                                          ? "text-red-400"
                                          : "text-zinc-300"
                                    }
                                  >
                                    {outcome.success_rate_delta > 0 ? "+" : ""}
                                    {outcome.success_rate_delta.toFixed(1)}pp
                                  </span>
                                </div>
                              )}
                              {outcome.duration_delta_ms != null && (
                                <div>
                                  <span className="text-zinc-500">Duration: </span>
                                  <span className="text-zinc-300">
                                    {outcome.duration_delta_ms > 0 ? "+" : ""}
                                    {outcome.duration_delta_ms.toFixed(0)}ms
                                  </span>
                                </div>
                              )}
                              {outcome.cost_delta_usd != null && (
                                <div>
                                  <span className="text-zinc-500">Cost: </span>
                                  <span className="text-zinc-300">
                                    {outcome.cost_delta_usd > 0 ? "+" : ""}$
                                    {outcome.cost_delta_usd.toFixed(4)}
                                  </span>
                                </div>
                              )}
                              {outcome.p_value != null && (
                                <div>
                                  <span className="text-zinc-500">p-value: </span>
                                  <span className="text-zinc-300">
                                    {outcome.p_value.toFixed(4)}
                                  </span>
                                </div>
                              )}
                              {outcome.confidence_interval != null && (
                                <div>
                                  <span className="text-zinc-500">CI: </span>
                                  <span className="text-zinc-300">
                                    [{outcome.confidence_interval[0].toFixed(2)},{" "}
                                    {outcome.confidence_interval[1].toFixed(2)}]
                                  </span>
                                </div>
                              )}
                              {outcome.effect_size != null && (
                                <div>
                                  <span className="text-zinc-500">Effect Size: </span>
                                  <span className="text-zinc-300">
                                    {outcome.effect_size.toFixed(3)}
                                  </span>
                                </div>
                              )}
                            </div>
                          </div>
                        );
                      } catch {
                        return null;
                      }
                    })()}

                  {/* Cascade impact for applied recs with target_agent */}
                  {rec.status === "applied" &&
                    rec.target_agent &&
                    cascadeEffects[rec.id] &&
                    cascadeEffects[rec.id].length > 0 && (
                      <div className="bg-zinc-800/50 rounded p-3 space-y-2">
                        <div className="text-xs text-zinc-500">Cascade Impact</div>
                        <table className="w-full text-xs">
                          <thead>
                            <tr className="text-zinc-500">
                              <th className="text-left py-1">Agent</th>
                              <th className="text-right py-1">Before</th>
                              <th className="text-right py-1">After</th>
                              <th className="text-right py-1">Delta</th>
                            </tr>
                          </thead>
                          <tbody>
                            {cascadeEffects[rec.id].map((ce) => (
                              <tr key={ce.affected_agent} className="border-t border-zinc-700/50">
                                <td className="py-1 text-zinc-300">{ce.affected_agent}</td>
                                <td className="py-1 text-right text-zinc-400">
                                  {ce.before_success_rate.toFixed(1)}%
                                </td>
                                <td className="py-1 text-right text-zinc-400">
                                  {ce.after_success_rate.toFixed(1)}%
                                </td>
                                <td
                                  className={`py-1 text-right ${ce.delta > 0 ? "text-green-400" : ce.delta < -2 ? "text-red-400" : "text-zinc-400"}`}
                                >
                                  {ce.delta > 0 ? "+" : ""}
                                  {ce.delta.toFixed(1)}pp
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    )}

                  {/* Eval results detail */}
                  {evalResults[rec.id] && evalResults[rec.id].length > 0 && (
                    <div className="bg-zinc-800/50 rounded p-3 space-y-2">
                      <div className="text-xs text-zinc-500">Eval Results</div>
                      {evalResults[rec.id].map((er) => (
                        <div key={er.id} className="space-y-2">
                          <div className="flex gap-4 text-xs">
                            <div>
                              <span className="text-zinc-500">Status: </span>
                              <span
                                className={
                                  er.status === "passed"
                                    ? "text-green-400"
                                    : er.status === "failed"
                                      ? "text-red-400"
                                      : "text-yellow-400"
                                }
                              >
                                {er.status}
                              </span>
                            </div>
                            <div>
                              <span className="text-zinc-500">Trials: </span>
                              <span className="text-zinc-300">{er.trials_run}</span>
                            </div>
                            <div>
                              <span className="text-zinc-500">Date: </span>
                              <span className="text-zinc-400">
                                {new Date(er.created_at).toLocaleDateString()}
                              </span>
                            </div>
                          </div>
                          {er.comparison && (
                            <div className="flex gap-4 text-xs">
                              <div>
                                <span className="text-zinc-500">Delta: </span>
                                <span
                                  className={
                                    er.comparison.success_rate_delta_pp > 0
                                      ? "text-green-400"
                                      : er.comparison.success_rate_delta_pp < -2
                                        ? "text-red-400"
                                        : "text-zinc-300"
                                  }
                                >
                                  {er.comparison.success_rate_delta_pp > 0 ? "+" : ""}
                                  {er.comparison.success_rate_delta_pp.toFixed(1)}pp
                                </span>
                              </div>
                              {er.comparison.p_value != null && (
                                <div>
                                  <span className="text-zinc-500">p-value: </span>
                                  <span className="text-zinc-300">
                                    {er.comparison.p_value.toFixed(4)}
                                  </span>
                                </div>
                              )}
                              {er.comparison.effect_size != null && (
                                <div>
                                  <span className="text-zinc-500">Effect: </span>
                                  <span className="text-zinc-300">
                                    {er.comparison.effect_size.toFixed(3)}
                                  </span>
                                </div>
                              )}
                              <div>
                                <span className="text-zinc-500">Verdict: </span>
                                <span className="text-zinc-300">{er.comparison.verdict}</span>
                              </div>
                            </div>
                          )}
                          {er.test_case_results.length > 0 && (
                            <div className="space-y-1">
                              {er.test_case_results.map((tc, i) => (
                                <div key={i} className="flex items-center gap-2 text-xs">
                                  <span
                                    className={`px-1 py-0.5 rounded ${tc.passed ? "bg-green-900/30 text-green-400" : "bg-red-900/30 text-red-400"}`}
                                  >
                                    {tc.passed ? "PASS" : "FAIL"}
                                  </span>
                                  <span className="text-zinc-400 font-mono text-[10px]">
                                    {tc.test_case_id.slice(0, 8)}
                                  </span>
                                  {tc.assertion_results
                                    .filter((ar) => !ar.passed)
                                    .map((ar, j) => (
                                      <span key={j} className="text-red-400">
                                        {ar.message}
                                      </span>
                                    ))}
                                </div>
                              ))}
                            </div>
                          )}
                        </div>
                      ))}
                    </div>
                  )}

                  <div className="flex gap-2 pt-2">
                    {rec.status === "pending" && (
                      <>
                        <button
                          onClick={() => handleAction(rec.id, "apply")}
                          className="px-3 py-1 text-xs bg-green-800 text-green-200 rounded hover:bg-green-700"
                        >
                          Apply
                        </button>
                        <button
                          onClick={() => handleAction(rec.id, "canary")}
                          className="px-3 py-1 text-xs bg-purple-800 text-purple-200 rounded hover:bg-purple-700"
                        >
                          Canary 10%
                        </button>
                        <button
                          onClick={() => handleAction(rec.id, "reject")}
                          className="px-3 py-1 text-xs bg-zinc-700 text-zinc-300 rounded hover:bg-zinc-600"
                        >
                          Reject
                        </button>
                        <button
                          onClick={() => handleRunEval(rec.id)}
                          disabled={runningEval === rec.id}
                          className="px-3 py-1 text-xs bg-cyan-800 text-cyan-200 rounded hover:bg-cyan-700 disabled:opacity-40 disabled:cursor-not-allowed"
                        >
                          {runningEval === rec.id ? "Evaluating..." : "Evaluate"}
                        </button>
                        {rec.recommendation_type === "prompt_rewrite" && (
                          <button
                            onClick={() => handleRunRobustness(rec)}
                            disabled={runningRobustness === rec.id}
                            className="px-3 py-1 text-xs bg-zinc-700 text-zinc-200 rounded hover:bg-zinc-600 disabled:opacity-40 disabled:cursor-not-allowed"
                          >
                            {runningRobustness === rec.id ? "Testing..." : "Run Robustness"}
                          </button>
                        )}
                      </>
                    )}
                    {rec.status === "applied" && (
                      <>
                        <button
                          onClick={() => handleAction(rec.id, "rollback")}
                          className="px-3 py-1 text-xs bg-orange-800 text-orange-200 rounded hover:bg-orange-700"
                        >
                          Rollback
                        </button>
                        <button
                          onClick={() => handleReevaluate(rec.id)}
                          className="px-3 py-1 text-xs bg-zinc-700 text-zinc-300 rounded hover:bg-zinc-600"
                        >
                          Re-evaluate
                        </button>
                      </>
                    )}
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

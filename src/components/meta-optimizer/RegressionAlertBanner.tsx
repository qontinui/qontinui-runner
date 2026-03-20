/**
 * RegressionAlertBanner — Red banner when applied recommendations show regression.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface OutcomeRec {
  id: string;
  title: string;
  target_agent: string | null;
  outcome_parsed?: {
    verdict: string;
    success_rate_delta?: number;
    duration_delta_ms?: number;
    cost_delta_usd?: number;
  };
}

export function RegressionAlertBanner() {
  const [regressions, setRegressions] = useState<OutcomeRec[]>([]);
  const [expanded, setExpanded] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [reevaluating, setReevaluating] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const outcomes = await invoke<OutcomeRec[]>("get_recommendation_outcomes");
      const regressed = outcomes.filter((r) => r.outcome_parsed?.verdict === "regressed");
      setRegressions(regressed);
    } catch {
      // silently fail
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const handleReevaluate = async (id: string) => {
    try {
      setReevaluating(id);
      await invoke("reevaluate_recommendation_outcome", { recommendationId: id });
      await load();
    } catch (e) {
      console.error("Re-evaluate failed:", e);
    } finally {
      setReevaluating(null);
    }
  };

  const handleRollback = async (id: string) => {
    try {
      await invoke("rollback_meta_optimizer_recommendation", { recommendationId: id });
      await load();
    } catch (e) {
      console.error("Rollback failed:", e);
    }
  };

  if (dismissed || regressions.length === 0) {
    return null;
  }

  return (
    <div className="bg-red-900/30 border border-red-800 rounded-lg p-3 mb-4">
      <div className="flex items-center justify-between">
        <div
          className="flex items-center gap-2 cursor-pointer flex-1"
          onClick={() => setExpanded(!expanded)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              setExpanded(!expanded);
            }
          }}
          role="button"
          tabIndex={0}
        >
          <span className="text-red-400 font-medium text-sm">
            {regressions.length} applied recommendation(s) show regression
          </span>
          <span className="text-red-600 text-xs">{expanded ? "\u25B2" : "\u25BC"}</span>
        </div>
        <button
          onClick={() => setDismissed(true)}
          className="text-red-600 hover:text-red-400 text-xs px-2"
        >
          Dismiss
        </button>
      </div>

      {expanded && (
        <div className="mt-3 space-y-2">
          {regressions.map((rec) => (
            <div key={rec.id} className="bg-red-950/50 rounded p-3 border border-red-900/50">
              <div className="flex items-center justify-between mb-2">
                <div>
                  <span className="text-sm text-red-200">{rec.title}</span>
                  {rec.target_agent && (
                    <span className="text-xs text-red-400 ml-2">({rec.target_agent})</span>
                  )}
                </div>
                <div className="flex gap-2">
                  <button
                    onClick={() => handleReevaluate(rec.id)}
                    disabled={reevaluating === rec.id}
                    className="px-2 py-1 text-xs bg-zinc-700 text-zinc-300 rounded hover:bg-zinc-600 disabled:opacity-50"
                  >
                    {reevaluating === rec.id ? "..." : "Re-evaluate"}
                  </button>
                  <button
                    onClick={() => handleRollback(rec.id)}
                    className="px-2 py-1 text-xs bg-orange-800 text-orange-200 rounded hover:bg-orange-700"
                  >
                    Rollback
                  </button>
                </div>
              </div>
              {rec.outcome_parsed && (
                <div className="flex gap-4 text-xs text-red-300">
                  {rec.outcome_parsed.success_rate_delta != null && (
                    <span>
                      Success Rate: {rec.outcome_parsed.success_rate_delta > 0 ? "+" : ""}
                      {rec.outcome_parsed.success_rate_delta.toFixed(1)}pp
                    </span>
                  )}
                  {rec.outcome_parsed.duration_delta_ms != null && (
                    <span>
                      Duration: {rec.outcome_parsed.duration_delta_ms > 0 ? "+" : ""}
                      {rec.outcome_parsed.duration_delta_ms.toFixed(0)}ms
                    </span>
                  )}
                  {rec.outcome_parsed.cost_delta_usd != null && (
                    <span>
                      Cost: {rec.outcome_parsed.cost_delta_usd > 0 ? "+" : ""}$
                      {rec.outcome_parsed.cost_delta_usd.toFixed(4)}
                    </span>
                  )}
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

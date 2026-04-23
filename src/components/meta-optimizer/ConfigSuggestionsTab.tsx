/**
 * ConfigSuggestionsTab — Architecture config recommendations with current vs. recommended.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

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
  created_at: string;
}

export function ConfigSuggestionsTab() {
  const [recs, setRecs] = useState<Recommendation[]>([]);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<Recommendation[]>("get_meta_optimizer_recommendations", {
        optimizerType: "architecture",
        status: null,
      });
      setRecs(result.filter((r) => r.recommendation_type === "config_change"));
    } catch (e) {
      console.error("Failed to load config suggestions:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    // Defer via microtask so the effect body itself doesn't synchronously
    // trigger setState inside load.
    void Promise.resolve().then(() => {
      if (!cancelled) void load();
    });
    return () => {
      cancelled = true;
    };
  }, [load]);

  const handleAction = async (id: string, action: "apply" | "reject" | "rollback") => {
    try {
      const cmd =
        action === "apply"
          ? "apply_meta_optimizer_recommendation"
          : action === "reject"
            ? "reject_meta_optimizer_recommendation"
            : "rollback_meta_optimizer_recommendation";
      await invoke(cmd, { recommendationId: id });
      load();
    } catch (e) {
      console.error(`Failed to ${action}:`, e);
    }
  };

  return (
    <div className="p-4 space-y-4">
      <div className="flex items-center gap-3">
        <h2 className="text-sm font-medium text-zinc-200">Architecture & Config Changes</h2>
        <button
          onClick={load}
          className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1 ml-auto"
        >
          Refresh
        </button>
      </div>

      {loading ? (
        <div className="text-zinc-500 text-sm">Loading...</div>
      ) : recs.length === 0 ? (
        <div className="text-zinc-500 text-sm py-8 text-center">
          No config change recommendations. The Architecture Optimizer will produce them after
          analyzing enough runs.
        </div>
      ) : (
        <div className="space-y-3">
          {recs.map((rec) => (
            <div
              key={rec.id}
              className="bg-zinc-900 border border-zinc-800 rounded-lg p-4 space-y-3"
            >
              <div className="flex items-center gap-3">
                <span
                  className={`text-xs px-2 py-0.5 rounded ${
                    rec.status === "pending"
                      ? "bg-yellow-900/30 text-yellow-400"
                      : rec.status === "applied"
                        ? "bg-green-900/30 text-green-400"
                        : "bg-zinc-800/50 text-zinc-500"
                  }`}
                >
                  {rec.status}
                </span>
                <span className="text-sm text-zinc-200">{rec.title}</span>
                <span className="text-xs text-zinc-500 ml-auto">
                  {(rec.confidence * 100).toFixed(0)}% confidence
                </span>
              </div>

              <p className="text-sm text-zinc-400">{rec.description}</p>

              {(rec.current_value || rec.recommended_value) && (
                <div className="grid grid-cols-2 gap-3">
                  {rec.current_value && (
                    <div>
                      <div className="text-xs text-zinc-500 mb-1">Current</div>
                      <pre className="text-xs text-red-300/70 bg-red-900/10 p-2 rounded overflow-auto max-h-32">
                        {rec.current_value}
                      </pre>
                    </div>
                  )}
                  {rec.recommended_value && (
                    <div>
                      <div className="text-xs text-zinc-500 mb-1">Recommended</div>
                      <pre className="text-xs text-green-300/70 bg-green-900/10 p-2 rounded overflow-auto max-h-32">
                        {rec.recommended_value}
                      </pre>
                    </div>
                  )}
                </div>
              )}

              <div className="flex gap-2">
                {rec.status === "pending" && (
                  <>
                    <button
                      onClick={() => handleAction(rec.id, "apply")}
                      className="px-3 py-1 text-xs bg-green-800 text-green-200 rounded hover:bg-green-700"
                    >
                      Apply
                    </button>
                    <button
                      onClick={() => handleAction(rec.id, "reject")}
                      className="px-3 py-1 text-xs bg-zinc-700 text-zinc-300 rounded hover:bg-zinc-600"
                    >
                      Reject
                    </button>
                  </>
                )}
                {rec.status === "applied" && (
                  <button
                    onClick={() => handleAction(rec.id, "rollback")}
                    className="px-3 py-1 text-xs bg-orange-800 text-orange-200 rounded hover:bg-orange-700"
                  >
                    Rollback
                  </button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

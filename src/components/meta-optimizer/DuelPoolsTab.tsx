/**
 * DuelPoolsTab — Browse duel pools, candidates with Copeland scores, and duel results.
 */

import { useState, useEffect, useCallback } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

interface DuelPool {
  id: string;
  agent_type: string;
  status: string;
  generation: number;
  config_json: string;
  created_at: string;
  completed_at: string;
}

interface DuelCandidate {
  id: string;
  pool_id: string;
  prompt_content: string;
  variant_id: string | null;
  generation: number;
  parent_id: string | null;
  copeland_score: number;
  alpha: number;
  beta: number;
  status: string;
  created_at: string;
}

interface DuelResult {
  id: string;
  pool_id: string;
  candidate_a_id: string;
  candidate_b_id: string;
  winner_id: string;
  judge_rationale: string | null;
  position_swapped: boolean;
  confidence: number;
  created_at: string;
}

interface PoolDetail {
  pool: DuelPool;
  candidates: DuelCandidate[];
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const STATUS_COLORS: Record<string, string> = {
  active: "text-green-400 bg-green-900/30",
  complete: "text-blue-400 bg-blue-900/30",
  converged: "text-blue-400 bg-blue-900/30",
  seeding: "text-yellow-400 bg-yellow-900/30",
  failed: "text-red-400 bg-red-900/30",
};

const CANDIDATE_STATUS_COLORS: Record<string, string> = {
  active: "text-green-400 bg-green-900/30",
  eliminated: "text-zinc-500 bg-zinc-800/50",
  champion: "text-amber-400 bg-amber-900/30",
};

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

async function fetchApi<T>(path: string): Promise<T | null> {
  try {
    const resp = await tracedFetch(`${getApiBase()}/meta-optimizer/${path}`);
    if (!resp.ok) return null;
    const json: ApiResponse<T> = await resp.json();
    return json.success ? (json.data ?? null) : null;
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function DuelPoolsTab() {
  const [pools, setPools] = useState<DuelPool[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedPoolId, setSelectedPoolId] = useState<string | null>(null);
  const [poolDetail, setPoolDetail] = useState<PoolDetail | null>(null);
  const [duelResults, setDuelResults] = useState<DuelResult[]>([]);
  const [detailLoading, setDetailLoading] = useState(false);
  const [showResults, setShowResults] = useState(false);
  const [expandedCandidate, setExpandedCandidate] = useState<string | null>(null);

  const loadPools = useCallback(async () => {
    try {
      setLoading(true);
      const data = await fetchApi<DuelPool[]>("duel-pools?limit=50");
      setPools(data ?? []);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadPools();
  }, [loadPools]);

  const selectPool = useCallback(async (poolId: string) => {
    setSelectedPoolId(poolId);
    setShowResults(false);
    setDetailLoading(true);
    try {
      const detail = await fetchApi<PoolDetail>(`duel-pools/${poolId}`);
      setPoolDetail(detail);
    } finally {
      setDetailLoading(false);
    }
  }, []);

  const loadResults = useCallback(async (poolId: string) => {
    setShowResults(true);
    const results = await fetchApi<DuelResult[]>(`duel-pools/${poolId}/results?limit=200`);
    setDuelResults(results ?? []);
  }, []);

  // Short ID helper
  const shortId = (id: string) => id.slice(0, 8);

  return (
    <div className="p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center gap-3">
        <h2 className="text-sm font-medium text-zinc-200">Duel Pools</h2>
        <button onClick={loadPools} className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1">
          Refresh
        </button>
        <span className="text-xs text-zinc-500 ml-auto">{pools.length} pools</span>
      </div>

      {loading ? (
        <div className="text-zinc-500 text-sm">Loading...</div>
      ) : pools.length === 0 ? (
        <div className="text-zinc-500 text-sm py-8 text-center">
          No duel pools yet. Duel pools are created by the optimization pipeline during prompt
          evolution.
        </div>
      ) : (
        <div className="flex gap-4">
          {/* Pool list */}
          <div className="w-80 shrink-0 space-y-1">
            {pools.map((pool) => (
              <div
                key={pool.id}
                onClick={() => selectPool(pool.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    selectPool(pool.id);
                  }
                }}
                role="button"
                tabIndex={0}
                className={`px-3 py-2 rounded-lg cursor-pointer border transition-colors ${
                  selectedPoolId === pool.id
                    ? "bg-zinc-800 border-zinc-600"
                    : "bg-zinc-900 border-zinc-800 hover:bg-zinc-800/50"
                }`}
              >
                <div className="flex items-center gap-2">
                  <span
                    className={`text-xs px-1.5 py-0.5 rounded ${STATUS_COLORS[pool.status] || "text-zinc-400 bg-zinc-800/50"}`}
                  >
                    {pool.status}
                  </span>
                  <span className="text-sm text-zinc-200">{pool.agent_type}</span>
                  <span className="text-xs text-zinc-500 ml-auto">gen {pool.generation}</span>
                </div>
                <div className="flex items-center gap-2 mt-1">
                  <span className="text-xs text-zinc-600 font-mono">{shortId(pool.id)}</span>
                  <span className="text-xs text-zinc-500 ml-auto">
                    {new Date(pool.created_at).toLocaleDateString()}
                  </span>
                </div>
              </div>
            ))}
          </div>

          {/* Pool detail */}
          <div className="flex-1 min-w-0">
            {selectedPoolId == null ? (
              <div className="text-zinc-500 text-sm py-8 text-center">
                Select a pool to view candidates and results.
              </div>
            ) : detailLoading ? (
              <div className="text-zinc-500 text-sm">Loading pool detail...</div>
            ) : poolDetail ? (
              <div className="space-y-4">
                {/* Pool info */}
                <div className="bg-zinc-900 border border-zinc-800 rounded-lg p-3">
                  <div className="flex items-center gap-3 text-sm">
                    <span className="text-zinc-200 font-medium">{poolDetail.pool.agent_type}</span>
                    <span
                      className={`text-xs px-1.5 py-0.5 rounded ${STATUS_COLORS[poolDetail.pool.status] || ""}`}
                    >
                      {poolDetail.pool.status}
                    </span>
                    <span className="text-xs text-zinc-500">
                      Generation {poolDetail.pool.generation}
                    </span>
                    <span className="text-xs text-zinc-500 ml-auto">
                      {poolDetail.candidates.length} candidates
                    </span>
                  </div>
                </div>

                {/* Candidates table (Copeland ranking) */}
                <div>
                  <div className="flex items-center gap-2 mb-2">
                    <h3 className="text-xs text-zinc-400 font-medium">
                      Candidates (Copeland Ranking)
                    </h3>
                  </div>
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="text-zinc-500 text-xs border-b border-zinc-800">
                        <th className="text-left py-2 px-2">#</th>
                        <th className="text-left py-2 px-2">ID</th>
                        <th className="text-left py-2 px-2">Status</th>
                        <th className="text-right py-2 px-2">Copeland</th>
                        <th className="text-right py-2 px-2">Alpha</th>
                        <th className="text-right py-2 px-2">Beta</th>
                        <th className="text-right py-2 px-2">Win Rate</th>
                        <th className="text-left py-2 px-2">Gen</th>
                        <th className="text-left py-2 px-2">Variant</th>
                      </tr>
                    </thead>
                    <tbody>
                      {poolDetail.candidates.map((c, i) => {
                        const winRate =
                          c.alpha + c.beta > 0
                            ? ((c.alpha / (c.alpha + c.beta)) * 100).toFixed(1)
                            : "—";
                        return (
                          <tr
                            key={c.id}
                            className="border-b border-zinc-800/50 hover:bg-zinc-800/30 cursor-pointer"
                            onClick={() =>
                              setExpandedCandidate(expandedCandidate === c.id ? null : c.id)
                            }
                          >
                            <td className="py-1.5 px-2 text-zinc-500 text-xs">{i + 1}</td>
                            <td className="py-1.5 px-2 text-zinc-400 font-mono text-xs">
                              {shortId(c.id)}
                            </td>
                            <td className="py-1.5 px-2">
                              <span
                                className={`text-xs px-1.5 py-0.5 rounded ${CANDIDATE_STATUS_COLORS[c.status] || "text-zinc-400 bg-zinc-800/50"}`}
                              >
                                {c.status}
                              </span>
                            </td>
                            <td className="py-1.5 px-2 text-right text-zinc-200 font-mono">
                              {c.copeland_score.toFixed(2)}
                            </td>
                            <td className="py-1.5 px-2 text-right text-zinc-400 text-xs">
                              {c.alpha.toFixed(1)}
                            </td>
                            <td className="py-1.5 px-2 text-right text-zinc-400 text-xs">
                              {c.beta.toFixed(1)}
                            </td>
                            <td className="py-1.5 px-2 text-right text-zinc-300 text-xs">
                              {winRate}%
                            </td>
                            <td className="py-1.5 px-2 text-zinc-500 text-xs">{c.generation}</td>
                            <td className="py-1.5 px-2 text-zinc-500 text-xs font-mono">
                              {c.variant_id ? shortId(c.variant_id) : "—"}
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>

                  {/* Expanded candidate prompt */}
                  {expandedCandidate &&
                    (() => {
                      const c = poolDetail.candidates.find((x) => x.id === expandedCandidate);
                      if (!c) return null;
                      return (
                        <div className="mt-2 bg-zinc-800/50 rounded p-3 space-y-2">
                          <div className="flex items-center gap-2 text-xs text-zinc-500">
                            <span>Candidate {shortId(c.id)}</span>
                            {c.parent_id && <span>parent: {shortId(c.parent_id)}</span>}
                          </div>
                          <pre className="text-xs text-zinc-300 bg-zinc-900/50 p-2 rounded overflow-auto max-h-48 whitespace-pre-wrap">
                            {c.prompt_content}
                          </pre>
                        </div>
                      );
                    })()}
                </div>

                {/* Duel results toggle */}
                <div>
                  <button
                    onClick={() => loadResults(selectedPoolId)}
                    className="text-xs text-zinc-400 hover:text-zinc-200 px-3 py-1.5 rounded border border-zinc-700 hover:border-zinc-600"
                  >
                    {showResults ? "Refresh Results" : "Show Duel Results"}
                  </button>

                  {showResults && (
                    <div className="mt-3">
                      {duelResults.length === 0 ? (
                        <div className="text-zinc-500 text-xs py-4 text-center">
                          No duel results for this pool.
                        </div>
                      ) : (
                        <table className="w-full text-sm">
                          <thead>
                            <tr className="text-zinc-500 text-xs border-b border-zinc-800">
                              <th className="text-left py-2 px-2">Candidate A</th>
                              <th className="text-left py-2 px-2">Candidate B</th>
                              <th className="text-left py-2 px-2">Winner</th>
                              <th className="text-right py-2 px-2">Confidence</th>
                              <th className="text-center py-2 px-2">Swapped</th>
                              <th className="text-left py-2 px-2">Date</th>
                            </tr>
                          </thead>
                          <tbody>
                            {duelResults.map((r) => (
                              <tr
                                key={r.id}
                                className="border-b border-zinc-800/50 hover:bg-zinc-800/30"
                              >
                                <td className="py-1.5 px-2 text-zinc-400 font-mono text-xs">
                                  {shortId(r.candidate_a_id)}
                                </td>
                                <td className="py-1.5 px-2 text-zinc-400 font-mono text-xs">
                                  {shortId(r.candidate_b_id)}
                                </td>
                                <td className="py-1.5 px-2 text-green-400 font-mono text-xs">
                                  {shortId(r.winner_id)}
                                </td>
                                <td className="py-1.5 px-2 text-right text-zinc-300 text-xs">
                                  {(r.confidence * 100).toFixed(0)}%
                                </td>
                                <td className="py-1.5 px-2 text-center text-xs text-zinc-500">
                                  {r.position_swapped ? "yes" : "no"}
                                </td>
                                <td className="py-1.5 px-2 text-zinc-500 text-xs">
                                  {new Date(r.created_at).toLocaleString()}
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      )}
                    </div>
                  )}
                </div>
              </div>
            ) : (
              <div className="text-zinc-500 text-sm py-8 text-center">
                Failed to load pool detail.
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

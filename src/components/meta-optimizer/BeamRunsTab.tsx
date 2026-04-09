/**
 * BeamRunsTab — Browse beam search runs and their candidates.
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

interface BeamRun {
  id: string;
  agent_type: string;
  pool_id: string | null;
  config_json: string;
  generation: number;
  status: string;
  created_at: string;
  completed_at: string;
}

interface BeamCandidate {
  id: string;
  beam_run_id: string;
  parent_id: string | null;
  prompt_content: string;
  critique: string | null;
  changes_summary: string | null;
  generation: number;
  thinking_style: string | null;
  status: string;
  created_at: string;
}

interface BeamRunDetail {
  beam_run_id: string;
  candidates: BeamCandidate[];
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const STATUS_COLORS: Record<string, string> = {
  active: "text-green-400 bg-green-900/30",
  running: "text-blue-400 bg-blue-900/30",
  complete: "text-blue-400 bg-blue-900/30",
  converged: "text-blue-400 bg-blue-900/30",
  failed: "text-red-400 bg-red-900/30",
  seeding: "text-yellow-400 bg-yellow-900/30",
};

const CANDIDATE_STATUS_COLORS: Record<string, string> = {
  active: "text-green-400 bg-green-900/30",
  pruned: "text-zinc-500 bg-zinc-800/50",
  selected: "text-amber-400 bg-amber-900/30",
  promoted: "text-cyan-400 bg-cyan-900/30",
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

export function BeamRunsTab() {
  const [runs, setRuns] = useState<BeamRun[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);
  const [candidates, setCandidates] = useState<BeamCandidate[]>([]);
  const [detailLoading, setDetailLoading] = useState(false);
  const [expandedCandidate, setExpandedCandidate] = useState<string | null>(null);

  const loadRuns = useCallback(async () => {
    try {
      setLoading(true);
      const data = await fetchApi<BeamRun[]>("beam-runs?limit=50");
      setRuns(data ?? []);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadRuns();
  }, [loadRuns]);

  const selectRun = useCallback(async (runId: string) => {
    setSelectedRunId(runId);
    setExpandedCandidate(null);
    setDetailLoading(true);
    try {
      const detail = await fetchApi<BeamRunDetail>(`beam-runs/${runId}`);
      setCandidates(detail?.candidates ?? []);
    } finally {
      setDetailLoading(false);
    }
  }, []);

  const shortId = (id: string) => id.slice(0, 8);

  // Group candidates by generation for a clearer view
  const generationGroups = candidates.reduce<Record<number, BeamCandidate[]>>((acc, c) => {
    if (!acc[c.generation]) acc[c.generation] = [];
    acc[c.generation].push(c);
    return acc;
  }, {});
  const sortedGenerations = Object.keys(generationGroups)
    .map(Number)
    .sort((a, b) => a - b);

  return (
    <div className="p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center gap-3">
        <h2 className="text-sm font-medium text-zinc-200">Beam Search Runs</h2>
        <button
          onClick={loadRuns}
          className="text-sm text-zinc-400 hover:text-zinc-200 px-2 py-1"
        >
          Refresh
        </button>
        <span className="text-xs text-zinc-500 ml-auto">{runs.length} runs</span>
      </div>

      {loading ? (
        <div className="text-zinc-500 text-sm">Loading...</div>
      ) : runs.length === 0 ? (
        <div className="text-zinc-500 text-sm py-8 text-center">
          No beam search runs yet. Beam search is triggered by the optimization pipeline to explore
          prompt variations.
        </div>
      ) : (
        <div className="flex gap-4">
          {/* Run list */}
          <div className="w-80 shrink-0 space-y-1">
            {runs.map((run) => (
              <div
                key={run.id}
                onClick={() => selectRun(run.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    selectRun(run.id);
                  }
                }}
                role="button"
                tabIndex={0}
                className={`px-3 py-2 rounded-lg cursor-pointer border transition-colors ${
                  selectedRunId === run.id
                    ? "bg-zinc-800 border-zinc-600"
                    : "bg-zinc-900 border-zinc-800 hover:bg-zinc-800/50"
                }`}
              >
                <div className="flex items-center gap-2">
                  <span
                    className={`text-xs px-1.5 py-0.5 rounded ${STATUS_COLORS[run.status] || "text-zinc-400 bg-zinc-800/50"}`}
                  >
                    {run.status}
                  </span>
                  <span className="text-sm text-zinc-200">{run.agent_type}</span>
                  <span className="text-xs text-zinc-500 ml-auto">gen {run.generation}</span>
                </div>
                <div className="flex items-center gap-2 mt-1">
                  <span className="text-xs text-zinc-600 font-mono">{shortId(run.id)}</span>
                  {run.pool_id && (
                    <span className="text-xs text-zinc-600">pool: {shortId(run.pool_id)}</span>
                  )}
                  <span className="text-xs text-zinc-500 ml-auto">
                    {new Date(run.created_at).toLocaleDateString()}
                  </span>
                </div>
              </div>
            ))}
          </div>

          {/* Run detail */}
          <div className="flex-1 min-w-0">
            {selectedRunId == null ? (
              <div className="text-zinc-500 text-sm py-8 text-center">
                Select a beam run to view its candidates.
              </div>
            ) : detailLoading ? (
              <div className="text-zinc-500 text-sm">Loading candidates...</div>
            ) : candidates.length === 0 ? (
              <div className="text-zinc-500 text-sm py-8 text-center">
                No candidates found for this beam run.
              </div>
            ) : (
              <div className="space-y-4">
                {/* Run info */}
                {(() => {
                  const run = runs.find((r) => r.id === selectedRunId);
                  if (!run) return null;
                  return (
                    <div className="bg-zinc-900 border border-zinc-800 rounded-lg p-3">
                      <div className="flex items-center gap-3 text-sm">
                        <span className="text-zinc-200 font-medium">{run.agent_type}</span>
                        <span
                          className={`text-xs px-1.5 py-0.5 rounded ${STATUS_COLORS[run.status] || ""}`}
                        >
                          {run.status}
                        </span>
                        <span className="text-xs text-zinc-500">
                          Generation {run.generation}
                        </span>
                        <span className="text-xs text-zinc-500 ml-auto">
                          {candidates.length} candidates across {sortedGenerations.length}{" "}
                          generation(s)
                        </span>
                      </div>
                    </div>
                  );
                })()}

                {/* Candidates grouped by generation */}
                {sortedGenerations.map((gen) => (
                  <div key={gen}>
                    <h3 className="text-xs text-zinc-500 font-medium mb-2">
                      Generation {gen} ({generationGroups[gen].length} candidates)
                    </h3>
                    <div className="space-y-1">
                      {generationGroups[gen].map((c) => (
                        <div
                          key={c.id}
                          className="bg-zinc-900 border border-zinc-800 rounded-lg overflow-hidden"
                        >
                          <div
                            className="flex items-center gap-3 px-3 py-2 cursor-pointer hover:bg-zinc-800/50"
                            onClick={() =>
                              setExpandedCandidate(expandedCandidate === c.id ? null : c.id)
                            }
                            onKeyDown={(e) => {
                              if (e.key === "Enter" || e.key === " ") {
                                e.preventDefault();
                                setExpandedCandidate(expandedCandidate === c.id ? null : c.id);
                              }
                            }}
                            role="button"
                            tabIndex={0}
                          >
                            <span className="text-xs text-zinc-500 font-mono">
                              {shortId(c.id)}
                            </span>
                            <span
                              className={`text-xs px-1.5 py-0.5 rounded ${CANDIDATE_STATUS_COLORS[c.status] || "text-zinc-400 bg-zinc-800/50"}`}
                            >
                              {c.status}
                            </span>
                            {c.thinking_style && (
                              <span className="text-xs text-purple-400 bg-purple-900/20 px-1.5 py-0.5 rounded">
                                {c.thinking_style}
                              </span>
                            )}
                            {c.changes_summary && (
                              <span className="text-xs text-zinc-300 truncate flex-1">
                                {c.changes_summary}
                              </span>
                            )}
                            {c.parent_id && (
                              <span className="text-xs text-zinc-600">
                                parent: {shortId(c.parent_id)}
                              </span>
                            )}
                          </div>

                          {expandedCandidate === c.id && (
                            <div className="px-3 pb-3 space-y-2 border-t border-zinc-800">
                              {c.critique && (
                                <div className="mt-2">
                                  <div className="text-xs text-zinc-500 mb-1">Critique</div>
                                  <p className="text-xs text-zinc-300 bg-zinc-800/50 p-2 rounded">
                                    {c.critique}
                                  </p>
                                </div>
                              )}
                              {c.changes_summary && (
                                <div>
                                  <div className="text-xs text-zinc-500 mb-1">Changes Summary</div>
                                  <p className="text-xs text-zinc-300 bg-zinc-800/50 p-2 rounded">
                                    {c.changes_summary}
                                  </p>
                                </div>
                              )}
                              <div>
                                <div className="text-xs text-zinc-500 mb-1">Prompt Content</div>
                                <pre className="text-xs text-zinc-300 bg-zinc-900/50 p-2 rounded overflow-auto max-h-48 whitespace-pre-wrap">
                                  {c.prompt_content}
                                </pre>
                              </div>
                            </div>
                          )}
                        </div>
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

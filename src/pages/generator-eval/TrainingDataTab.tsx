import { useState } from "react";
import { BarChart, Bar, XAxis, YAxis, Tooltip, ResponsiveContainer, CartesianGrid } from "recharts";
import { fetchApi, type TrainingExample } from "./types";

const SCORE_COLORS: Record<string, string> = {
  high: "bg-green-500/20 text-green-400",
  medium: "bg-amber-500/20 text-amber-400",
  low: "bg-red-500/20 text-red-400",
};

function scoreClass(score: number): string {
  if (score >= 0.8) return SCORE_COLORS.high;
  if (score >= 0.5) return SCORE_COLORS.medium;
  return SCORE_COLORS.low;
}

export function TrainingDataTab() {
  const [agent, setAgent] = useState("builder");
  const [minScore, setMinScore] = useState(0);
  const [limit, setLimit] = useState(50);
  const [data, setData] = useState<TrainingExample[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expandedRow, setExpandedRow] = useState<number | null>(null);

  const loadData = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await fetchApi<TrainingExample[]>(
        `/generator-eval/training-data?agent=${agent}&min_score=${minScore}&limit=${limit}`,
      );
      setData(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load");
    } finally {
      setLoading(false);
    }
  };

  // Score distribution buckets
  const scoreBuckets = Array.from({ length: 10 }, (_, i) => {
    const low = i / 10;
    const high = (i + 1) / 10;
    const count = data.filter((d) => d.score >= low && d.score < high).length;
    return { bucket: `${(low * 100).toFixed(0)}-${(high * 100).toFixed(0)}%`, count };
  });

  return (
    <div className="space-y-6">
      {/* Filter controls */}
      <div className="flex items-end gap-3 flex-wrap">
        <div>
          <label className="text-xs text-muted-foreground block mb-1">Agent</label>
          <select
            value={agent}
            onChange={(e) => setAgent(e.target.value)}
            className="text-xs bg-background border border-border rounded px-2 py-1"
          >
            <option value="specification">Specification</option>
            <option value="builder">Builder</option>
            <option value="hardener">Hardener</option>
          </select>
        </div>
        <div>
          <label className="text-xs text-muted-foreground block mb-1">Min Score</label>
          <input
            type="number"
            min={0}
            max={1}
            step={0.1}
            value={minScore}
            onChange={(e) => setMinScore(Number(e.target.value))}
            className="text-xs bg-background border border-border rounded px-2 py-1 w-20"
          />
        </div>
        <div>
          <label className="text-xs text-muted-foreground block mb-1">Limit</label>
          <input
            type="number"
            min={1}
            max={500}
            value={limit}
            onChange={(e) => setLimit(Number(e.target.value))}
            className="text-xs bg-background border border-border rounded px-2 py-1 w-20"
          />
        </div>
        <button
          onClick={loadData}
          disabled={loading}
          className="text-xs px-3 py-1 rounded bg-purple-500/20 text-purple-400 border border-purple-500/30 hover:bg-purple-500/30 transition-colors disabled:opacity-50"
        >
          {loading ? "Loading..." : "Fetch"}
        </button>
      </div>

      {error && (
        <div className="text-red-400 text-sm">
          Error: {error}{" "}
          <button onClick={loadData} className="underline ml-2">
            Retry
          </button>
        </div>
      )}

      {data.length === 0 && !loading && !error && (
        <div className="text-sm text-muted-foreground text-center py-8">
          No training data yet. Click Fetch to load data, or generate and run workflows first.
        </div>
      )}

      {data.length > 0 && (
        <>
          {/* Score distribution chart */}
          <div className="rounded-lg border border-border bg-card p-4">
            <div className="text-xs text-muted-foreground mb-2">Score Distribution</div>
            <ResponsiveContainer width="100%" height={150}>
              <BarChart data={scoreBuckets}>
                <CartesianGrid strokeDasharray="3 3" stroke="#333" />
                <XAxis dataKey="bucket" tick={{ fontSize: 9 }} stroke="#666" />
                <YAxis tick={{ fontSize: 10 }} stroke="#666" />
                <Tooltip
                  contentStyle={{
                    background: "#1a1a2e",
                    border: "1px solid #333",
                    borderRadius: 6,
                    fontSize: 12,
                  }}
                />
                <Bar dataKey="count" fill="#8b5cf6" radius={[4, 4, 0, 0]} />
              </BarChart>
            </ResponsiveContainer>
          </div>

          {/* Results table */}
          <div className="rounded-lg border border-border overflow-auto max-h-[400px]">
            <table className="w-full text-xs">
              <thead className="bg-muted/30 sticky top-0">
                <tr>
                  <th className="text-left px-3 py-1.5">Score</th>
                  <th className="text-left px-3 py-1.5">Agent</th>
                  <th className="text-left px-3 py-1.5">Artifact</th>
                  <th className="text-left px-3 py-1.5">Workflow</th>
                  <th className="text-left px-3 py-1.5">Date</th>
                </tr>
              </thead>
              <tbody>
                {data.map((example, i) => (
                  <>
                    <tr
                      key={i}
                      className="border-t border-border hover:bg-muted/20 cursor-pointer"
                      onClick={() => setExpandedRow(expandedRow === i ? null : i)}
                    >
                      <td className="px-3 py-1.5">
                        <span
                          className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${scoreClass(example.score)}`}
                        >
                          {(example.score * 100).toFixed(0)}%
                        </span>
                      </td>
                      <td className="px-3 py-1.5 capitalize">{example.agent}</td>
                      <td className="px-3 py-1.5 font-mono text-muted-foreground">
                        {example.artifact_id.slice(0, 12)}
                      </td>
                      <td className="px-3 py-1.5 text-muted-foreground">
                        {example.workflow_id ? example.workflow_id.slice(0, 8) : "\u2014"}
                      </td>
                      <td className="px-3 py-1.5 text-muted-foreground">
                        {new Date(example.created_at).toLocaleDateString()}
                      </td>
                    </tr>
                    {expandedRow === i && (
                      <tr key={`${i}-expanded`} className="border-t border-border/50">
                        <td colSpan={5} className="px-3 py-2 bg-muted/10">
                          <div className="space-y-2">
                            <div>
                              <div className="text-[10px] text-muted-foreground mb-1">
                                Prompt (first 500 chars)
                              </div>
                              <pre className="font-mono text-[10px] whitespace-pre-wrap bg-background/50 p-2 rounded max-h-32 overflow-auto">
                                {example.prompt.slice(0, 500)}
                              </pre>
                            </div>
                            <div>
                              <div className="text-[10px] text-muted-foreground mb-1">
                                Completion (first 500 chars)
                              </div>
                              <pre className="font-mono text-[10px] whitespace-pre-wrap bg-background/50 p-2 rounded max-h-32 overflow-auto">
                                {example.completion.slice(0, 500)}
                              </pre>
                            </div>
                          </div>
                        </td>
                      </tr>
                    )}
                  </>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </div>
  );
}

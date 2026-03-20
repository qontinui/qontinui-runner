import React, { useEffect, useState } from "react";
import { RefreshCw } from "lucide-react";
import { fetchApi, type PromptInsight } from "./types";

const AGENT_COLORS: Record<string, string> = {
  specification: "bg-purple-500/20 text-purple-400",
  builder: "bg-blue-500/20 text-blue-400",
  verification: "bg-amber-500/20 text-amber-400",
  hardener: "bg-green-500/20 text-green-400",
};

export function InsightsTab() {
  const [data, setData] = useState<PromptInsight[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expandedRow, setExpandedRow] = useState<number | null>(null);

  const loadData = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await fetchApi<PromptInsight[]>("/generator-eval/insights");
      setData(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-32 text-muted-foreground">
        Loading insights...
      </div>
    );
  }
  if (error) {
    return (
      <div className="text-red-400 p-4">
        Error: {error}{" "}
        <button onClick={loadData} className="underline ml-2">
          Retry
        </button>
      </div>
    );
  }
  if (data.length === 0) {
    return (
      <div className="text-sm text-muted-foreground text-center py-8">
        No insights yet. Generate and run workflows to accumulate analysis data.
      </div>
    );
  }

  // Count insights per agent
  const agentCounts: Record<string, number> = {};
  for (const insight of data) {
    agentCounts[insight.agent] = (agentCounts[insight.agent] || 0) + 1;
  }

  return (
    <div className="space-y-6">
      <div className="flex justify-end">
        <button
          onClick={loadData}
          className="flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
        >
          <RefreshCw className="w-3 h-3" />
          Refresh
        </button>
      </div>

      {/* Summary cards */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        {(["specification", "builder", "verification", "hardener"] as const).map((agent) => (
          <div key={agent} className="rounded-lg border border-border bg-card p-3 text-center">
            <div className="text-2xl font-bold">{agentCounts[agent] || 0}</div>
            <div className="text-xs text-muted-foreground capitalize">{agent}</div>
          </div>
        ))}
      </div>

      {/* Insights table */}
      <div className="rounded-lg border border-border overflow-auto max-h-[500px]">
        <table className="w-full text-xs">
          <thead className="bg-muted/30 sticky top-0">
            <tr>
              <th className="text-left px-3 py-1.5">Agent</th>
              <th className="text-left px-3 py-1.5">Type</th>
              <th className="text-left px-3 py-1.5">Description</th>
              <th className="text-right px-3 py-1.5">Evidence</th>
              <th className="text-right px-3 py-1.5">Confidence</th>
            </tr>
          </thead>
          <tbody>
            {data.map((insight, i) => (
              <React.Fragment key={`${insight.agent}-${insight.insight_type}-${i}`}>
                <tr
                  className="border-t border-border hover:bg-muted/20 cursor-pointer"
                  onClick={() => setExpandedRow(expandedRow === i ? null : i)}
                >
                  <td className="px-3 py-1.5">
                    <span
                      className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${
                        AGENT_COLORS[insight.agent] || "bg-gray-500/20 text-gray-400"
                      }`}
                    >
                      {insight.agent}
                    </span>
                  </td>
                  <td className="px-3 py-1.5">
                    <span className="px-1.5 py-0.5 rounded text-[10px] bg-muted text-muted-foreground">
                      {insight.insight_type}
                    </span>
                  </td>
                  <td className="px-3 py-1.5 max-w-[400px] truncate">{insight.description}</td>
                  <td className="px-3 py-1.5 text-right text-muted-foreground">
                    {insight.evidence_count}
                  </td>
                  <td className="px-3 py-1.5 text-right">
                    <span
                      className={
                        insight.confidence >= 0.8
                          ? "text-green-400"
                          : insight.confidence >= 0.5
                            ? "text-amber-400"
                            : "text-red-400"
                      }
                    >
                      {(insight.confidence * 100).toFixed(0)}%
                    </span>
                  </td>
                </tr>
                {expandedRow === i && insight.suggested_rule && (
                  <tr className="border-t border-border/50">
                    <td colSpan={5} className="px-3 py-2 bg-muted/10">
                      <div className="text-[10px] text-muted-foreground mb-1">Suggested Rule</div>
                      <div className="font-mono text-xs whitespace-pre-wrap">
                        {insight.suggested_rule}
                      </div>
                    </td>
                  </tr>
                )}
              </React.Fragment>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

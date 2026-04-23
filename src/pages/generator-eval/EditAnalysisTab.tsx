import { useEffect, useState } from "react";
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  CartesianGrid,
  PieChart,
  Pie,
  Cell,
} from "recharts";
import { RefreshCw } from "lucide-react";
import { fetchApi, type EditAnalysis } from "./types";

const COLORS = ["#8b5cf6", "#ef4444", "#f59e0b", "#10b981", "#06b6d4"];

function formatDate(iso: string): string {
  return new Date(iso).toLocaleString();
}

export function EditAnalysisTab() {
  const [data, setData] = useState<EditAnalysis | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadData = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await fetchApi<EditAnalysis>("/generator-eval/edit-analysis");
      setData(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    // Async IIFE: avoid invoking a setState-bearing callback synchronously
    // in the effect body (set-state-in-effect).
    let cancelled = false;
    void (async () => {
      if (cancelled) return;
      await loadData();
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-32 text-muted-foreground">
        Loading edit analysis...
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
  if (!data) return null;

  const hasData =
    data.edited_fields.length > 0 ||
    data.type_distribution.length > 0 ||
    data.recent_feedback.length > 0;

  if (!hasData) {
    return (
      <div className="text-sm text-muted-foreground text-center py-8">
        No feedback data yet. Edit, rate, or delete generated workflows to see analysis here.
      </div>
    );
  }

  const fieldChartData = data.edited_fields.map(([field, count]) => ({
    field,
    count,
  }));

  const typeChartData = data.type_distribution.map(([type, count]) => ({
    name: type,
    value: count,
  }));

  const ratingChartData = data.rating_distribution.map(([rating, count]) => ({
    rating: `${rating}`,
    count,
  }));

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

      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        {/* Most edited fields */}
        {fieldChartData.length > 0 && (
          <div className="rounded-lg border border-border bg-card p-4">
            <div className="text-xs text-muted-foreground mb-2">Most Edited Fields</div>
            <ResponsiveContainer width="100%" height={200}>
              <BarChart data={fieldChartData} layout="vertical">
                <CartesianGrid strokeDasharray="3 3" stroke="#333" />
                <XAxis type="number" tick={{ fontSize: 10 }} stroke="#666" />
                <YAxis
                  type="category"
                  dataKey="field"
                  tick={{ fontSize: 10 }}
                  stroke="#666"
                  width={100}
                />
                <Tooltip
                  contentStyle={{
                    background: "#1a1a2e",
                    border: "1px solid #333",
                    borderRadius: 6,
                    fontSize: 12,
                  }}
                />
                <Bar dataKey="count" fill="#8b5cf6" radius={[0, 4, 4, 0]} />
              </BarChart>
            </ResponsiveContainer>
          </div>
        )}

        {/* Feedback type distribution */}
        {typeChartData.length > 0 && (
          <div className="rounded-lg border border-border bg-card p-4">
            <div className="text-xs text-muted-foreground mb-2">Feedback Type Distribution</div>
            <ResponsiveContainer width="100%" height={200}>
              <PieChart>
                <Pie
                  data={typeChartData}
                  dataKey="value"
                  nameKey="name"
                  cx="50%"
                  cy="50%"
                  outerRadius={70}
                  label={({ name, percent }: { name?: string; percent?: number }) =>
                    `${name ?? ""} ${((percent ?? 0) * 100).toFixed(0)}%`
                  }
                  labelLine={false}
                  fontSize={10}
                >
                  {typeChartData.map((entry, i) => (
                    <Cell key={`${entry.name}-${i}`} fill={COLORS[i % COLORS.length]} />
                  ))}
                </Pie>
                <Tooltip
                  contentStyle={{
                    background: "#1a1a2e",
                    border: "1px solid #333",
                    borderRadius: 6,
                    fontSize: 12,
                  }}
                />
              </PieChart>
            </ResponsiveContainer>
          </div>
        )}

        {/* Rating distribution */}
        {ratingChartData.length > 0 && (
          <div className="rounded-lg border border-border bg-card p-4">
            <div className="text-xs text-muted-foreground mb-2">Rating Distribution</div>
            <ResponsiveContainer width="100%" height={200}>
              <BarChart data={ratingChartData}>
                <CartesianGrid strokeDasharray="3 3" stroke="#333" />
                <XAxis dataKey="rating" tick={{ fontSize: 10 }} stroke="#666" />
                <YAxis tick={{ fontSize: 10 }} stroke="#666" />
                <Tooltip
                  contentStyle={{
                    background: "#1a1a2e",
                    border: "1px solid #333",
                    borderRadius: 6,
                    fontSize: 12,
                  }}
                />
                <Bar dataKey="count" fill="#f59e0b" radius={[4, 4, 0, 0]} />
              </BarChart>
            </ResponsiveContainer>
          </div>
        )}
      </div>

      {/* Recent feedback table */}
      {data.recent_feedback.length > 0 && (
        <div>
          <h2 className="text-sm font-semibold mb-2">Recent Feedback</h2>
          <div className="rounded-lg border border-border overflow-auto max-h-[400px]">
            <table className="w-full text-xs">
              <thead className="bg-muted/30 sticky top-0">
                <tr>
                  <th className="text-left px-3 py-1.5">Workflow</th>
                  <th className="text-left px-3 py-1.5">Type</th>
                  <th className="text-left px-3 py-1.5">Field</th>
                  <th className="text-left px-3 py-1.5">Date</th>
                </tr>
              </thead>
              <tbody>
                {data.recent_feedback.map((fb) => (
                  <tr key={fb.id} className="border-t border-border hover:bg-muted/20">
                    <td className="px-3 py-1.5 truncate max-w-[200px]">
                      {fb.workflow_name || fb.workflow_id.slice(0, 8)}
                    </td>
                    <td className="px-3 py-1.5">
                      <span
                        className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${
                          fb.feedback_type === "edit"
                            ? "bg-blue-500/20 text-blue-400"
                            : fb.feedback_type === "delete"
                              ? "bg-red-500/20 text-red-400"
                              : fb.feedback_type === "rating"
                                ? "bg-yellow-500/20 text-yellow-400"
                                : "bg-gray-500/20 text-gray-400"
                        }`}
                      >
                        {fb.feedback_type}
                      </span>
                    </td>
                    <td className="px-3 py-1.5 text-muted-foreground">{fb.edited_field || "—"}</td>
                    <td className="px-3 py-1.5 text-muted-foreground">
                      {formatDate(fb.created_at).split(",")[0]}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}

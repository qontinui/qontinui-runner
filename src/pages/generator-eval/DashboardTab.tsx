import { useCallback, useEffect, useRef, useState } from "react";
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  CartesianGrid,
} from "recharts";
import { RefreshCw } from "lucide-react";
import { fetchApi, type DashboardMetrics, type TimeSeriesPoint } from "./types";

function MetricCard({
  label,
  value,
  subtext,
}: {
  label: string;
  value: string | number;
  subtext?: string;
}) {
  return (
    <div className="rounded-lg border border-border bg-card p-3">
      <div className="text-xs text-muted-foreground mb-1">{label}</div>
      <div className="text-xl font-semibold">{value}</div>
      {subtext && <div className="text-xs text-muted-foreground mt-0.5">{subtext}</div>}
    </div>
  );
}

const AUTO_REFRESH_INTERVAL_MS = 10_000;

export function DashboardTab() {
  const [metrics, setMetrics] = useState<DashboardMetrics | null>(null);
  const [trends, setTrends] = useState<TimeSeriesPoint[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [autoRefresh, setAutoRefresh] = useState(false);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const loadData = useCallback(async (silent = false) => {
    if (!silent) {
      setLoading(true);
    }
    setError(null);
    try {
      const [m, t] = await Promise.all([
        fetchApi<DashboardMetrics>("/generator-eval/dashboard"),
        fetchApi<TimeSeriesPoint[]>("/generator-eval/trends?days=30"),
      ]);
      setMetrics(m);
      setTrends(t);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load");
    } finally {
      if (!silent) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    loadData();
  }, [loadData]);

  // Auto-refresh polling
  useEffect(() => {
    if (autoRefresh) {
      intervalRef.current = setInterval(() => {
        loadData(true);
      }, AUTO_REFRESH_INTERVAL_MS);
    }
    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
    };
  }, [autoRefresh, loadData]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-32 text-muted-foreground">
        Loading metrics...
      </div>
    );
  }
  if (error) {
    return (
      <div className="text-red-400 p-4">
        Error: {error}{" "}
        <button onClick={() => loadData()} className="underline ml-2">
          Retry
        </button>
      </div>
    );
  }
  if (!metrics) return null;

  const fmtPct = (v: number) => `${(v * 100).toFixed(1)}%`;
  const fmtDuration = (ms: number | null) => {
    if (ms == null) return "—";
    if (ms < 1000) return `${Math.round(ms)}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
  };

  return (
    <div className="space-y-6">
      {/* Refresh controls */}
      <div className="flex items-center justify-end gap-3">
        <label className="flex items-center gap-1.5 text-xs text-muted-foreground cursor-pointer select-none">
          <input
            type="checkbox"
            checked={autoRefresh}
            onChange={(e) => setAutoRefresh(e.target.checked)}
            className="accent-primary h-3 w-3 cursor-pointer"
          />
          Auto-refresh
          {autoRefresh && (
            <span className="relative flex h-2 w-2 ml-0.5">
              <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-75" />
              <span className="relative inline-flex rounded-full h-2 w-2 bg-emerald-500" />
            </span>
          )}
        </label>
        <button
          onClick={() => loadData()}
          className="flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
        >
          <RefreshCw className="w-3 h-3" />
          Refresh
        </button>
      </div>

      {/* Metric cards */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <MetricCard label="Total Generations" value={metrics.total_generations} />
        <MetricCard
          label="Success Rate"
          value={fmtPct(metrics.success_rate)}
          subtext={`${metrics.successful_generations} / ${metrics.total_generations}`}
        />
        <MetricCard label="Avg Duration" value={fmtDuration(metrics.avg_total_duration_ms)} />
        <MetricCard
          label="Avg Fix Iterations"
          value={metrics.avg_verification_iterations?.toFixed(1) ?? "—"}
        />
        <MetricCard
          label="First-Pass Rate"
          value={metrics.first_pass_rate != null ? fmtPct(metrics.first_pass_rate) : "—"}
          subtext="Pass verification on first try"
        />
        <MetricCard
          label="Hardener Conversions"
          value={metrics.hardener_total_converted}
          subtext={`of ${metrics.hardener_total_processed} processed`}
        />
        <MetricCard
          label="User Edits"
          value={metrics.total_edits}
          subtext={`${metrics.total_deletes} deletes`}
        />
        <MetricCard
          label="Avg Rating"
          value={metrics.avg_rating != null ? metrics.avg_rating.toFixed(1) : "—"}
          subtext={`${metrics.total_ratings} ratings`}
        />
      </div>

      {/* Trend charts */}
      {trends.length > 1 && (
        <div className="space-y-4">
          <h2 className="text-sm font-semibold">Trends (30 days)</h2>

          <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
            {/* Generations over time */}
            <div className="rounded-lg border border-border bg-card p-4">
              <div className="text-xs text-muted-foreground mb-2">Generations / Day</div>
              <ResponsiveContainer width="100%" height={180}>
                <LineChart data={trends}>
                  <CartesianGrid strokeDasharray="3 3" stroke="#333" />
                  <XAxis dataKey="date" tick={{ fontSize: 10 }} stroke="#666" />
                  <YAxis tick={{ fontSize: 10 }} stroke="#666" />
                  <Tooltip
                    contentStyle={{
                      background: "#1a1a2e",
                      border: "1px solid #333",
                      borderRadius: 6,
                      fontSize: 12,
                    }}
                  />
                  <Line
                    type="monotone"
                    dataKey="total_generations"
                    stroke="#8b5cf6"
                    strokeWidth={2}
                    dot={false}
                    name="Total"
                  />
                  <Line
                    type="monotone"
                    dataKey="successful_generations"
                    stroke="#10b981"
                    strokeWidth={2}
                    dot={false}
                    name="Successful"
                  />
                </LineChart>
              </ResponsiveContainer>
            </div>

            {/* Avg duration over time */}
            <div className="rounded-lg border border-border bg-card p-4">
              <div className="text-xs text-muted-foreground mb-2">Avg Duration (ms) / Day</div>
              <ResponsiveContainer width="100%" height={180}>
                <LineChart data={trends}>
                  <CartesianGrid strokeDasharray="3 3" stroke="#333" />
                  <XAxis dataKey="date" tick={{ fontSize: 10 }} stroke="#666" />
                  <YAxis tick={{ fontSize: 10 }} stroke="#666" />
                  <Tooltip
                    contentStyle={{
                      background: "#1a1a2e",
                      border: "1px solid #333",
                      borderRadius: 6,
                      fontSize: 12,
                    }}
                  />
                  <Line
                    type="monotone"
                    dataKey="avg_duration_ms"
                    stroke="#f59e0b"
                    strokeWidth={2}
                    dot={false}
                    name="Avg Duration (ms)"
                  />
                </LineChart>
              </ResponsiveContainer>
            </div>
          </div>
        </div>
      )}

      {trends.length === 0 && (
        <div className="text-sm text-muted-foreground text-center py-8">
          No trend data yet. Generate some workflows to see trends.
        </div>
      )}
    </div>
  );
}

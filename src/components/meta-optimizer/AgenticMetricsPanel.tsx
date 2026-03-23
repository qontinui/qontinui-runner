/**
 * AgenticMetricsPanel — Shows agentic metric scores, trends, and per-metric breakdowns.
 *
 * Displays: composite score trend chart, per-metric aggregates table,
 * and per-run metric breakdown when a task run is selected.
 */

import { useState, useEffect, useCallback } from "react";
import {
  agenticMetricsService,
  AgenticMetricAggregate,
  AgenticMetricScore,
  CompositeScoreTrendPoint,
  METRIC_INFO,
} from "../../services/agentic-metrics-service";

interface AgenticMetricsPanelProps {
  /** Optional task_run_id to show per-run breakdown. */
  selectedTaskRunId?: string | null;
  /** Time range in days for aggregates and trend. */
  days?: number;
}

/** Format a 0-1 score as a colored percentage string. */
function formatScore(score: number): string {
  return `${(score * 100).toFixed(0)}%`;
}

/** Get a color class based on score value. */
function scoreColor(score: number): string {
  if (score >= 0.8) return "text-green-400";
  if (score >= 0.6) return "text-yellow-400";
  if (score >= 0.4) return "text-orange-400";
  return "text-red-400";
}

/** Get a background color for the score bar. */
function scoreBarColor(score: number): string {
  if (score >= 0.8) return "bg-green-500";
  if (score >= 0.6) return "bg-yellow-500";
  if (score >= 0.4) return "bg-orange-500";
  return "bg-red-500";
}

export function AgenticMetricsPanel({
  selectedTaskRunId,
  days = 30,
}: AgenticMetricsPanelProps) {
  const [aggregates, setAggregates] = useState<AgenticMetricAggregate[]>([]);
  const [trend, setTrend] = useState<CompositeScoreTrendPoint[]>([]);
  const [runScores, setRunScores] = useState<AgenticMetricScore[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadData = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [agg, trendData] = await Promise.all([
        agenticMetricsService.getAggregates(days),
        agenticMetricsService.getCompositeScoreTrend(days),
      ]);
      setAggregates(agg);
      setTrend(trendData);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [days]);

  // Load aggregates and trend on mount
  useEffect(() => {
    loadData();
  }, [loadData]);

  // Load per-run scores when a task run is selected
  useEffect(() => {
    if (!selectedTaskRunId) {
      setRunScores([]);
      return;
    }
    agenticMetricsService
      .getScoresForRun(selectedTaskRunId)
      .then(setRunScores)
      .catch(() => setRunScores([]));
  }, [selectedTaskRunId]);

  if (loading) {
    return (
      <div className="p-4 text-zinc-500 text-sm">Loading agentic metrics...</div>
    );
  }

  if (error) {
    return (
      <div className="p-4 text-red-400 text-sm">
        Failed to load metrics: {error}
      </div>
    );
  }

  const latestComposite =
    trend.length > 0 ? trend[trend.length - 1].avg_composite_score : null;
  const totalRuns = aggregates.reduce(
    (max, a) => Math.max(max, a.runs_scored),
    0
  );

  return (
    <div className="space-y-4">
      {/* Header with composite score */}
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium text-zinc-300">Agentic Metrics</h3>
        {latestComposite !== null && (
          <div className="flex items-center gap-2">
            <span className="text-xs text-zinc-500">Composite:</span>
            <span
              className={`text-lg font-bold ${scoreColor(latestComposite)}`}
            >
              {formatScore(latestComposite)}
            </span>
            <span className="text-xs text-zinc-600">
              ({totalRuns} runs scored)
            </span>
          </div>
        )}
      </div>

      {/* Composite trend sparkline */}
      {trend.length > 1 && <CompositeTrendChart data={trend} />}

      {/* Per-metric aggregates */}
      {aggregates.length > 0 && (
        <div className="space-y-1">
          <div className="text-xs text-zinc-500 mb-2">
            {days}-day averages by metric
          </div>
          {aggregates
            .sort((a, b) => {
              const infoA = METRIC_INFO[a.metric_type];
              const infoB = METRIC_INFO[b.metric_type];
              if (infoA?.category !== infoB?.category)
                return infoA?.category === "agentic" ? -1 : 1;
              return b.mean_score - a.mean_score;
            })
            .map((agg) => (
              <MetricRow key={agg.metric_type} aggregate={agg} />
            ))}
        </div>
      )}

      {aggregates.length === 0 && (
        <div className="text-xs text-zinc-600 p-2">
          No scored runs yet. Run a workflow to generate metrics.
        </div>
      )}

      {/* Per-run breakdown */}
      {selectedTaskRunId && runScores.length > 0 && (
        <div className="border-t border-zinc-800 pt-3">
          <div className="text-xs text-zinc-500 mb-2">
            Scores for {selectedTaskRunId.slice(0, 8)}...
          </div>
          {runScores.map((s) => (
            <RunScoreRow key={s.metric_type} score={s} />
          ))}
        </div>
      )}
    </div>
  );
}

/** A single row in the per-metric aggregates table. */
function MetricRow({ aggregate }: { aggregate: AgenticMetricAggregate }) {
  const info = METRIC_INFO[aggregate.metric_type];
  const label = info?.label ?? aggregate.metric_type;
  const isRag = info?.category === "rag";

  return (
    <div className="flex items-center gap-2 text-xs">
      <div
        className={`w-28 truncate ${isRag ? "text-blue-400" : "text-zinc-400"}`}
        title={info?.description}
      >
        {label}
      </div>
      <div className="flex-1 h-2 bg-zinc-800 rounded-full overflow-hidden">
        <div
          className={`h-full rounded-full ${scoreBarColor(aggregate.mean_score)}`}
          style={{ width: `${aggregate.mean_score * 100}%` }}
        />
      </div>
      <div className={`w-10 text-right ${scoreColor(aggregate.mean_score)}`}>
        {formatScore(aggregate.mean_score)}
      </div>
      <div className="w-8 text-right text-zinc-600">{aggregate.runs_scored}</div>
    </div>
  );
}

/** A single row in the per-run score breakdown. */
function RunScoreRow({ score }: { score: AgenticMetricScore }) {
  const info = METRIC_INFO[score.metric_type];
  const label = info?.label ?? score.metric_type;

  return (
    <div className="flex items-center gap-2 text-xs py-0.5">
      <div className="w-28 truncate text-zinc-400">{label}</div>
      <div className="flex-1 h-1.5 bg-zinc-800 rounded-full overflow-hidden">
        <div
          className={`h-full rounded-full ${scoreBarColor(score.score)}`}
          style={{ width: `${score.score * 100}%` }}
        />
      </div>
      <div className={`w-10 text-right ${scoreColor(score.score)}`}>
        {formatScore(score.score)}
      </div>
      <div className="w-4">
        {score.is_llm_judged && (
          <span title="LLM-judged" className="text-purple-400">
            *
          </span>
        )}
      </div>
      {score.rationale && (
        <div
          className="w-48 truncate text-zinc-600"
          title={score.rationale}
        >
          {score.rationale}
        </div>
      )}
    </div>
  );
}

/** Simple text-based composite score trend (CSS-only, no chart library). */
function CompositeTrendChart({
  data,
}: {
  data: CompositeScoreTrendPoint[];
}) {
  const scores = data.map((d) => d.avg_composite_score);
  const min = Math.min(...scores);
  const max = Math.max(...scores);
  const range = max - min || 0.1;

  // Show last 14 data points max
  const recent = data.slice(-14);

  return (
    <div className="flex items-end gap-px h-10">
      {recent.map((point, i) => {
        const height = ((point.avg_composite_score - min) / range) * 100;
        const isLast = i === recent.length - 1;
        return (
          <div
            key={point.date}
            className={`flex-1 rounded-t ${
              isLast ? "bg-blue-400" : "bg-zinc-700"
            }`}
            style={{ height: `${Math.max(height, 8)}%` }}
            title={`${point.date}: ${formatScore(point.avg_composite_score)} (${point.run_count} runs)`}
          />
        );
      })}
    </div>
  );
}

/**
 * SpecCompliancePanel — Shows compliance gauge, severity breakdown, group scores,
 * and historical trend for a single spec or all specs.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface GroupScore {
  group_name: string;
  score: number;
  passed: number;
  total: number;
}

interface SpecComplianceResult {
  id: string;
  task_run_id: string;
  spec_id: string | null;
  iteration: number;
  overall_score: number;
  raw_pass_rate: number;
  critical_passed: number;
  critical_total: number;
  warning_passed: number;
  warning_total: number;
  info_passed: number;
  info_total: number;
  assertions_passed: number;
  assertions_total: number;
  group_scores: GroupScore[];
  created_at: string;
}

interface SpecComplianceSummary {
  spec_id: string | null;
  latest_score: number;
  trend: string;
  run_count: number;
}

function scoreColor(score: number): string {
  if (score >= 0.9) return "text-green-400";
  if (score >= 0.7) return "text-yellow-400";
  return "text-red-400";
}

function trendIcon(trend: string): string {
  if (trend === "improving") return "\u2191";
  if (trend === "declining") return "\u2193";
  return "\u2192";
}

function trendColor(trend: string): string {
  if (trend === "improving") return "text-green-400";
  if (trend === "declining") return "text-red-400";
  return "text-zinc-500";
}

function SeverityBar({
  label,
  passed,
  total,
  color,
}: {
  label: string;
  passed: number;
  total: number;
  color: string;
}) {
  if (total === 0) return null;
  const pct = (passed / total) * 100;
  return (
    <div className="flex items-center gap-2 text-sm">
      <span className="w-16 text-zinc-500">{label}</span>
      <div className="flex-1 h-2 bg-zinc-700 rounded-full overflow-hidden">
        <div className={`h-full ${color} rounded-full`} style={{ width: `${pct}%` }} />
      </div>
      <span className="w-12 text-right text-zinc-400 text-xs">
        {passed}/{total}
      </span>
    </div>
  );
}

function Sparkline({ scores }: { scores: number[] }) {
  if (scores.length < 2) return null;
  const min = Math.min(...scores);
  const max = Math.max(...scores);
  const range = max - min || 1;
  const w = 120;
  const h = 24;
  const points = scores
    .map((s, i) => {
      const x = (i / (scores.length - 1)) * w;
      const y = h - ((s - min) / range) * (h - 4) - 2;
      return `${x},${y}`;
    })
    .join(" ");

  return (
    <svg width={w} height={h} className="inline-block">
      <polyline fill="none" stroke="#60a5fa" strokeWidth="1.5" points={points} />
    </svg>
  );
}

export function SpecCompliancePanel({ specId }: { specId?: string }) {
  const [history, setHistory] = useState<SpecComplianceResult[]>([]);
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      const result = await invoke<SpecComplianceResult[]>("get_spec_compliance_history", {
        specId: specId ?? null,
        limit: 20,
      });
      setHistory(result);
    } catch (e) {
      console.error("Failed to load compliance history:", e);
    } finally {
      setLoading(false);
    }
  }, [specId]);

  useEffect(() => {
    load();
  }, [load]);

  if (loading) {
    return <div className="p-4 text-zinc-500 text-sm">Loading compliance data...</div>;
  }

  if (history.length === 0) {
    return (
      <div className="p-4 text-zinc-500 text-sm">
        No compliance data available. Run spec workflows to generate compliance metrics.
      </div>
    );
  }

  const latest = history[0];
  const scorePct = (latest.overall_score * 100).toFixed(1);
  const sparkScores = [...history].reverse().map((h) => h.overall_score);

  return (
    <div className="space-y-4">
      {/* Compliance gauge */}
      <div className="bg-zinc-800 rounded-lg p-4 border border-zinc-700">
        <div className="flex items-center justify-between mb-3">
          <div className="text-xs text-zinc-500 uppercase tracking-wide">Spec Compliance</div>
          <Sparkline scores={sparkScores} />
        </div>
        <div className="flex items-end gap-4">
          <div className={`text-3xl font-bold ${scoreColor(latest.overall_score)}`}>
            {scorePct}%
          </div>
          <div className="text-xs text-zinc-500 mb-1">
            {latest.assertions_passed}/{latest.assertions_total} assertions passing
          </div>
        </div>
      </div>

      {/* Severity breakdown */}
      <div className="bg-zinc-800 rounded-lg p-4 border border-zinc-700 space-y-2">
        <div className="text-xs text-zinc-500 uppercase tracking-wide mb-2">Severity Breakdown</div>
        <SeverityBar
          label="Critical"
          passed={latest.critical_passed}
          total={latest.critical_total}
          color="bg-red-500"
        />
        <SeverityBar
          label="Warning"
          passed={latest.warning_passed}
          total={latest.warning_total}
          color="bg-yellow-500"
        />
        <SeverityBar
          label="Info"
          passed={latest.info_passed}
          total={latest.info_total}
          color="bg-blue-500"
        />
      </div>

      {/* Per-group scores */}
      {latest.group_scores.length > 0 && (
        <div className="bg-zinc-800 rounded-lg p-4 border border-zinc-700">
          <div className="text-xs text-zinc-500 uppercase tracking-wide mb-2">Group Scores</div>
          <table className="w-full text-sm">
            <thead>
              <tr className="text-zinc-500 text-xs border-b border-zinc-700">
                <th className="text-left py-1 px-2">Group</th>
                <th className="text-right py-1 px-2">Score</th>
                <th className="text-right py-1 px-2">Passed</th>
              </tr>
            </thead>
            <tbody>
              {latest.group_scores.map((g) => (
                <tr key={g.group_name} className="border-b border-zinc-800/50">
                  <td className="py-1 px-2 text-zinc-300">{g.group_name}</td>
                  <td className={`py-1 px-2 text-right ${scoreColor(g.score)}`}>
                    {(g.score * 100).toFixed(0)}%
                  </td>
                  <td className="py-1 px-2 text-right text-zinc-400">
                    {g.passed}/{g.total}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

export function SpecComplianceSummaryTable() {
  const [summaries, setSummaries] = useState<SpecComplianceSummary[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    invoke<SpecComplianceSummary[]>("get_spec_compliance_summary")
      .then(setSummaries)
      .catch((e) => console.error("Failed to load compliance summary:", e))
      .finally(() => setLoading(false));
  }, []);

  if (loading) {
    return <div className="text-zinc-500 text-sm">Loading...</div>;
  }

  if (summaries.length === 0) {
    return null;
  }

  return (
    <div className="bg-zinc-800 rounded-lg p-4 border border-zinc-700">
      <div className="text-xs text-zinc-500 uppercase tracking-wide mb-2">
        Spec Compliance Overview
      </div>
      <table className="w-full text-sm">
        <thead>
          <tr className="text-zinc-500 text-xs border-b border-zinc-700">
            <th className="text-left py-1 px-2">Spec</th>
            <th className="text-right py-1 px-2">Score</th>
            <th className="text-center py-1 px-2">Trend</th>
            <th className="text-right py-1 px-2">Runs</th>
          </tr>
        </thead>
        <tbody>
          {summaries.map((s) => (
            <tr key={s.spec_id ?? "all"} className="border-b border-zinc-800/50">
              <td className="py-1 px-2 text-zinc-300 truncate max-w-[200px]">
                {s.spec_id ?? "(all)"}
              </td>
              <td className={`py-1 px-2 text-right ${scoreColor(s.latest_score)}`}>
                {(s.latest_score * 100).toFixed(1)}%
              </td>
              <td className={`py-1 px-2 text-center ${trendColor(s.trend)}`}>
                {trendIcon(s.trend)} {s.trend}
              </td>
              <td className="py-1 px-2 text-right text-zinc-400">{s.run_count}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

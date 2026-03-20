/**
 * FailureAnalysisSection — In-depth failure breakdowns for the Meta-Optimizer Progress tab.
 *
 * Fetches failure analysis data (abort reasons, verification failures,
 * finding distributions, fix effectiveness, generation quality, pipeline
 * agent failures, recurring issues) and renders multiple panels.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

// ── Interfaces ──────────────────────────────────────────────────────────────

interface AbortReason {
  reason: string;
  count: number;
  percentage: number;
}

interface VerificationFailurePattern {
  iteration: number;
  total_checks: number;
  failed_checks: number;
  failure_rate: number;
  critical_failure_count: number;
}

interface CategoryCount {
  category: string;
  count: number;
}

interface FixEffectivenessRecord {
  fix_type: string;
  total: number;
  effective: number;
  ineffective: number;
  caused_regression: number;
  effectiveness_rate: number;
  source_agent: string | null;
}

interface GenerationQualityMetrics {
  total_feedback: number;
  edits: number;
  deletes: number;
  avg_rating: number | null;
  most_edited_fields: CategoryCount[];
  delete_reasons: CategoryCount[];
}

interface PipelineAgentFailureRecord {
  agent_type: string;
  total_runs: number;
  failures: number;
  failure_rate: number;
  avg_duration_ms: number;
  avg_cost_usd: number;
}

interface RecurringIssue {
  signature_hash: string;
  title: string;
  category: string;
  severity: string;
  occurrence_count: number;
  last_seen: string;
}

interface FailureAnalysis {
  period_days: number;
  total_runs: number;
  failed_runs: number;
  failure_rate: number;
  abort_reasons: AbortReason[];
  verification_failures: VerificationFailurePattern[];
  finding_distribution: CategoryCount[];
  severity_distribution: CategoryCount[];
  reflection_fix_effectiveness: FixEffectivenessRecord[];
  generation_quality: GenerationQualityMetrics;
  pipeline_agent_failures: PipelineAgentFailureRecord[];
  recurring_issues: RecurringIssue[];
}

// ── Helpers ─────────────────────────────────────────────────────────────────

function formatReasonName(raw: string): string {
  return raw.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
}

function rateColor(rate: number): string {
  // rate is already in 0-100 scale from backend
  if (rate > 30) return "text-red-400";
  if (rate > 10) return "text-yellow-400";
  return "text-green-400";
}

function effectivenessColor(rate: number): string {
  // rate is already in 0-100 scale from backend
  if (rate >= 80) return "text-green-400";
  if (rate >= 50) return "text-yellow-400";
  return "text-red-400";
}

const SEVERITY_DOT: Record<string, string> = {
  critical: "bg-red-500",
  high: "bg-orange-500",
  medium: "bg-yellow-500",
  low: "bg-blue-500",
  info: "bg-zinc-500",
};

function severityDot(severity: string): string {
  return SEVERITY_DOT[severity.toLowerCase()] ?? "bg-zinc-500";
}

function formatDurationMs(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${Math.round(ms)}ms`;
}

function formatCostUsd(usd: number): string {
  return `$${usd.toFixed(2)}`;
}

// ── Sub-components ──────────────────────────────────────────────────────────

function EmptyState({ message }: { message: string }) {
  return <div className="text-zinc-600 text-xs py-2">{message}</div>;
}

function AbortReasonsChart({ reasons }: { reasons: AbortReason[] }) {
  if (reasons.length === 0) {
    return <EmptyState message="No abort reasons recorded for this period" />;
  }

  const maxCount = Math.max(...reasons.map((r) => r.count));

  return (
    <div className="space-y-2">
      {reasons.map((r) => (
        <div key={r.reason} className="flex items-center gap-3 text-sm">
          <div className="w-44 text-zinc-300 truncate shrink-0">{formatReasonName(r.reason)}</div>
          <div className="flex-1 h-5 bg-zinc-800 rounded overflow-hidden">
            <div
              className="h-full bg-red-800 rounded"
              style={{ width: `${maxCount > 0 ? (r.count / maxCount) * 100 : 0}%` }}
            />
          </div>
          <div className="w-20 text-right text-zinc-400 text-xs shrink-0">
            {r.count} ({r.percentage.toFixed(0)}%)
          </div>
        </div>
      ))}
    </div>
  );
}

function VerificationFailuresTable({ failures }: { failures: VerificationFailurePattern[] }) {
  if (failures.length === 0) {
    return <EmptyState message="No verification failure data for this period" />;
  }

  return (
    <table className="w-full text-sm">
      <thead>
        <tr className="text-zinc-500 text-xs border-b border-zinc-800">
          <th className="text-left py-2 px-3">Iteration</th>
          <th className="text-right py-2 px-3">Total Checks</th>
          <th className="text-right py-2 px-3">Failed</th>
          <th className="text-right py-2 px-3">Failure Rate</th>
          <th className="text-right py-2 px-3">Critical</th>
        </tr>
      </thead>
      <tbody>
        {failures.map((f) => (
          <tr key={f.iteration} className="border-b border-zinc-800/50 hover:bg-zinc-800/30">
            <td className="py-2 px-3 text-zinc-200">{f.iteration}</td>
            <td className="py-2 px-3 text-right text-zinc-300">{f.total_checks}</td>
            <td className="py-2 px-3 text-right text-zinc-300">{f.failed_checks}</td>
            <td className={`py-2 px-3 text-right ${rateColor(f.failure_rate)}`}>
              {f.failure_rate.toFixed(1)}%
            </td>
            <td className="py-2 px-3 text-right text-zinc-300">{f.critical_failure_count}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function FindingDistributions({
  categories,
  severities,
}: {
  categories: CategoryCount[];
  severities: CategoryCount[];
}) {
  if (categories.length === 0 && severities.length === 0) {
    return <EmptyState message="No finding distribution data for this period" />;
  }

  return (
    <div className="grid grid-cols-2 gap-6">
      {/* Category distribution */}
      <div>
        <div className="text-xs text-zinc-500 mb-2">Category Distribution</div>
        {categories.length === 0 ? (
          <EmptyState message="No data for this period" />
        ) : (
          <ul className="space-y-1">
            {categories.map((c) => (
              <li key={c.category} className="flex items-center justify-between text-sm">
                <span className="text-zinc-300">{formatReasonName(c.category)}</span>
                <span className="text-zinc-400">{c.count}</span>
              </li>
            ))}
          </ul>
        )}
      </div>

      {/* Severity distribution */}
      <div>
        <div className="text-xs text-zinc-500 mb-2">Severity Distribution</div>
        {severities.length === 0 ? (
          <EmptyState message="No data for this period" />
        ) : (
          <ul className="space-y-1">
            {severities.map((s) => (
              <li key={s.category} className="flex items-center justify-between text-sm">
                <span className="flex items-center gap-2 text-zinc-300">
                  <span
                    className={`inline-block w-2 h-2 rounded-full ${severityDot(s.category)}`}
                  />
                  {formatReasonName(s.category)}
                </span>
                <span className="text-zinc-400">{s.count}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function FixEffectivenessTable({ records }: { records: FixEffectivenessRecord[] }) {
  if (records.length === 0) {
    return <EmptyState message="No fix effectiveness data for this period" />;
  }

  return (
    <table className="w-full text-sm">
      <thead>
        <tr className="text-zinc-500 text-xs border-b border-zinc-800">
          <th className="text-left py-2 px-3">Fix Type</th>
          <th className="text-left py-2 px-3">Agent</th>
          <th className="text-right py-2 px-3">Total</th>
          <th className="text-right py-2 px-3">Effective</th>
          <th className="text-right py-2 px-3">Ineffective</th>
          <th className="text-right py-2 px-3">Regression</th>
          <th className="text-right py-2 px-3">Rate</th>
        </tr>
      </thead>
      <tbody>
        {records.map((r, i) => (
          <tr
            key={`${r.fix_type}-${r.source_agent ?? i}`}
            className="border-b border-zinc-800/50 hover:bg-zinc-800/30"
          >
            <td className="py-2 px-3 text-zinc-200">{formatReasonName(r.fix_type)}</td>
            <td className="py-2 px-3 text-zinc-400">
              {r.source_agent ? formatReasonName(r.source_agent) : "--"}
            </td>
            <td className="py-2 px-3 text-right text-zinc-300">{r.total}</td>
            <td className="py-2 px-3 text-right text-zinc-300">{r.effective}</td>
            <td className="py-2 px-3 text-right text-zinc-300">{r.ineffective}</td>
            <td className="py-2 px-3 text-right text-zinc-300">{r.caused_regression}</td>
            <td className={`py-2 px-3 text-right ${effectivenessColor(r.effectiveness_rate)}`}>
              {r.effectiveness_rate.toFixed(1)}%
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function GenerationQuality({ quality }: { quality: GenerationQualityMetrics }) {
  if (quality.total_feedback === 0) {
    return <EmptyState message="No generation quality data for this period" />;
  }

  return (
    <div className="space-y-3">
      {/* Stat cards */}
      <div className="grid grid-cols-3 gap-3">
        <div className="bg-zinc-800 rounded px-3 py-2 border border-zinc-700">
          <div className="text-xs text-zinc-500">Edits</div>
          <div className="text-lg font-medium text-zinc-200">{quality.edits}</div>
        </div>
        <div className="bg-zinc-800 rounded px-3 py-2 border border-zinc-700">
          <div className="text-xs text-zinc-500">Deletes</div>
          <div className="text-lg font-medium text-zinc-200">{quality.deletes}</div>
        </div>
        <div className="bg-zinc-800 rounded px-3 py-2 border border-zinc-700">
          <div className="text-xs text-zinc-500">Avg Rating</div>
          <div className="text-lg font-medium text-zinc-200">
            {quality.avg_rating != null ? `${quality.avg_rating.toFixed(1)}/5` : "--"}
          </div>
        </div>
      </div>

      {/* Most Edited Fields */}
      {quality.most_edited_fields.length > 0 && (
        <div>
          <div className="text-xs text-zinc-500 mb-1">Most Edited Fields</div>
          <ul className="space-y-0.5">
            {quality.most_edited_fields.map((f) => (
              <li key={f.category} className="flex items-center justify-between text-sm">
                <span className="text-zinc-300">{f.category}</span>
                <span className="text-zinc-400">{f.count}</span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {/* Delete Reasons */}
      {quality.delete_reasons.length > 0 && (
        <div>
          <div className="text-xs text-zinc-500 mb-1">Delete Reasons</div>
          <ul className="space-y-0.5">
            {quality.delete_reasons.map((r) => (
              <li key={r.category} className="flex items-center justify-between text-sm">
                <span className="text-zinc-300">{formatReasonName(r.category)}</span>
                <span className="text-zinc-400">{r.count}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

function PipelineAgentFailuresTable({ agents }: { agents: PipelineAgentFailureRecord[] }) {
  if (agents.length === 0) return null;

  return (
    <div className="border-t border-zinc-800 pt-4 mt-4">
      <div className="text-sm font-medium text-zinc-200 mb-2">Pipeline Agent Performance</div>
      <table className="w-full text-sm">
        <thead>
          <tr className="text-zinc-500 text-xs border-b border-zinc-800">
            <th className="text-left py-2 px-3">Agent</th>
            <th className="text-right py-2 px-3">Runs</th>
            <th className="text-right py-2 px-3">Failures</th>
            <th className="text-right py-2 px-3">Failure Rate</th>
            <th className="text-right py-2 px-3">Avg Duration</th>
            <th className="text-right py-2 px-3">Avg Cost</th>
          </tr>
        </thead>
        <tbody>
          {agents.map((a) => (
            <tr key={a.agent_type} className="border-b border-zinc-800/50 hover:bg-zinc-800/30">
              <td className="py-2 px-3 text-zinc-200">{formatReasonName(a.agent_type)}</td>
              <td className="py-2 px-3 text-right text-zinc-300">{a.total_runs}</td>
              <td className="py-2 px-3 text-right text-zinc-300">{a.failures}</td>
              <td className={`py-2 px-3 text-right ${rateColor(a.failure_rate)}`}>
                {a.failure_rate.toFixed(1)}%
              </td>
              <td className="py-2 px-3 text-right text-zinc-300">
                {formatDurationMs(a.avg_duration_ms)}
              </td>
              <td className="py-2 px-3 text-right text-zinc-300">
                {formatCostUsd(a.avg_cost_usd)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function RecurringIssuesList({ issues }: { issues: RecurringIssue[] }) {
  if (issues.length === 0) return null;

  const sorted = [...issues].sort((a, b) => b.occurrence_count - a.occurrence_count).slice(0, 10);

  return (
    <div className="border-t border-zinc-800 pt-4 mt-4">
      <div className="text-sm font-medium text-zinc-200 mb-2">Recurring Issues</div>
      <ul className="space-y-2">
        {sorted.map((issue) => (
          <li key={issue.signature_hash} className="flex items-start gap-2 text-sm">
            <span
              className={`mt-1.5 inline-block w-2 h-2 rounded-full shrink-0 ${severityDot(issue.severity)}`}
            />
            <div className="flex-1 min-w-0">
              <div className="text-zinc-200 truncate">{issue.title}</div>
              <div className="flex items-center gap-2 mt-0.5">
                <span className="text-xs px-2 py-0.5 rounded bg-zinc-800 text-zinc-400">
                  {formatReasonName(issue.category)}
                </span>
                <span className="text-xs text-zinc-500">{issue.occurrence_count}x</span>
                <span className="text-xs text-zinc-600">
                  last seen {new Date(issue.last_seen).toLocaleDateString()}
                </span>
              </div>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

// ── Main component ──────────────────────────────────────────────────────────

interface FailureAnalysisSectionProps {
  category?: string;
}

export function FailureAnalysisSection({ category = "main" }: FailureAnalysisSectionProps) {
  const [data, setData] = useState<FailureAnalysis | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<FailureAnalysis>("get_meta_optimizer_failure_analysis", {
        days: 30,
        category,
      });
      setData(result);
    } catch (e) {
      console.error("Failed to load failure analysis:", e);
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [category]);

  useEffect(() => {
    load();
  }, [load]);

  if (loading) {
    return <div className="text-zinc-500 text-sm py-4">Loading failure analysis...</div>;
  }

  if (error && !data) {
    return (
      <div className="text-red-400 text-sm py-2">Failed to load failure analysis: {error}</div>
    );
  }

  if (!data || data.total_runs === 0) {
    return (
      <div className="text-zinc-500 text-sm py-4 text-center">
        No workflow runs in the last 30 days.
      </div>
    );
  }

  return (
    <div className="space-y-0">
      {/* A. Summary header bar */}
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-3">
          <span className="text-sm font-medium text-zinc-200">
            Failure Analysis (last 30 days)
            {category !== "main" && (
              <span className="text-xs text-zinc-500 ml-2">
                —{" "}
                {category === "reflections"
                  ? "reflections only"
                  : category === "fixers"
                    ? "fixers only"
                    : category === "follow_ups"
                      ? "follow-ups only"
                      : category === "meta_optimizer"
                        ? "meta-optimizer only"
                        : ""}
              </span>
            )}
          </span>
          <span className="text-xs text-zinc-400">
            {data.total_runs} total runs, {data.failed_runs} failed ({data.failure_rate.toFixed(1)}
            %)
          </span>
        </div>
        <button onClick={load} className="text-xs text-zinc-400 hover:text-zinc-200 px-2 py-1">
          Refresh
        </button>
      </div>

      <div className="bg-zinc-900 rounded-lg border border-zinc-800 p-4 space-y-0">
        {/* B. Abort Reasons */}
        <div>
          <div className="text-sm font-medium text-zinc-200 mb-2">
            Why Runs Fail — Abort Reasons
          </div>
          <AbortReasonsChart reasons={data.abort_reasons} />
        </div>

        {/* C. Verification Step Failures */}
        <div className="border-t border-zinc-800 pt-4 mt-4">
          <div className="text-sm font-medium text-zinc-200 mb-2">
            Verification Step Failures by Iteration
          </div>
          <VerificationFailuresTable failures={data.verification_failures} />
        </div>

        {/* D. Finding Categories */}
        <div className="border-t border-zinc-800 pt-4 mt-4">
          <div className="text-sm font-medium text-zinc-200 mb-2">Finding Categories</div>
          <FindingDistributions
            categories={data.finding_distribution}
            severities={data.severity_distribution}
          />
        </div>

        {/* E. Reflection Fix Effectiveness */}
        <div className="border-t border-zinc-800 pt-4 mt-4">
          <div className="text-sm font-medium text-zinc-200 mb-2">Reflection Fix Effectiveness</div>
          <FixEffectivenessTable records={data.reflection_fix_effectiveness} />
        </div>

        {/* F. Workflow Generation Quality */}
        <div className="border-t border-zinc-800 pt-4 mt-4">
          <div className="text-sm font-medium text-zinc-200 mb-2">Workflow Generation Quality</div>
          <GenerationQuality quality={data.generation_quality} />
        </div>

        {/* G. Pipeline Agent Performance (conditional) */}
        <PipelineAgentFailuresTable agents={data.pipeline_agent_failures} />

        {/* H. Recurring Issues (conditional) */}
        <RecurringIssuesList issues={data.recurring_issues} />
      </div>
    </div>
  );
}

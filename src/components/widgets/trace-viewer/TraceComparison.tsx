import React, { useState, useCallback } from "react";
import type { SpanMatch, ComparisonSummary } from "./types";
import { PHASE_COLORS } from "./types";
import { formatDuration, inferPhase } from "./trace-utils";
import { useTraceComparisonData } from "./useTraceComparisonData";

interface TraceComparisonProps {
  /** Current execution ID (Run B — the latest run). */
  currentExecutionId: string | null;
  /** Baseline execution ID (Run A — the previous run). */
  baselineExecutionId: string | null;
  /** Callback when user selects a different run. */
  onSelectRunA?: (id: string) => void;
  onSelectRunB?: (id: string) => void;
  /** Available run IDs for dropdowns. */
  availableRuns?: { id: string; label: string }[];
  height: number;
}

const STATUS_COLORS: Record<SpanMatch["status"], { bg: string; text: string; label: string }> = {
  unchanged: { bg: "bg-zinc-800", text: "text-zinc-400", label: "Unchanged" },
  faster: { bg: "bg-emerald-900/30", text: "text-emerald-400", label: "Faster" },
  slower: { bg: "bg-red-900/30", text: "text-red-400", label: "Slower" },
  added: { bg: "bg-amber-900/30", text: "text-amber-400", label: "Added" },
  removed: { bg: "bg-zinc-900/50", text: "text-zinc-500", label: "Removed" },
  status_changed: { bg: "bg-red-900/40", text: "text-red-300", label: "Status Changed" },
};

function SummaryBar({ summary }: { summary: ComparisonSummary }) {
  const deltaSign = summary.totalDeltaPct > 0 ? "+" : "";
  const deltaColor = summary.totalDeltaPct > 5 ? "text-red-400" : summary.totalDeltaPct < -5 ? "text-emerald-400" : "text-zinc-300";

  return (
    <div className="flex items-center gap-4 px-3 py-2 bg-zinc-800/50 border-b border-zinc-700 text-[11px]">
      <span className="text-zinc-400">
        Run A: <span className="text-zinc-200">{formatDuration(summary.totalDurationA)}</span>
      </span>
      <span className="text-zinc-400">
        Run B: <span className="text-zinc-200">{formatDuration(summary.totalDurationB)}</span>
      </span>
      <span className={deltaColor}>
        {deltaSign}{summary.totalDeltaPct.toFixed(1)}% overall
      </span>

      <span className="ml-auto flex gap-3">
        {summary.fasterCount > 0 && (
          <span className="text-emerald-400">{summary.fasterCount} faster</span>
        )}
        {summary.slowerCount > 0 && (
          <span className="text-red-400">{summary.slowerCount} slower</span>
        )}
        {summary.addedCount > 0 && (
          <span className="text-amber-400">{summary.addedCount} added</span>
        )}
        {summary.removedCount > 0 && (
          <span className="text-zinc-500">{summary.removedCount} removed</span>
        )}
        {summary.statusChangedCount > 0 && (
          <span className="text-red-300">{summary.statusChangedCount} status changed</span>
        )}
      </span>
    </div>
  );
}

function RegressionBanner({ regressions }: { regressions: SpanMatch[] }) {
  if (regressions.length === 0) return null;

  return (
    <div className="px-3 py-1.5 bg-red-900/20 border-b border-red-800/40 text-[11px] text-red-300">
      <span className="font-medium">Regressions detected:</span>{" "}
      {regressions.map((r, i) => {
        const span = r.spanB ?? r.spanA;
        const name = span?.name.replace(/^qontinui\./, "") ?? "unknown";
        return (
          <span key={i}>
            {i > 0 && ", "}
            {name}
            {r.durationDeltaPct !== null && ` (+${r.durationDeltaPct.toFixed(0)}%)`}
            {r.status === "status_changed" && " (now failing)"}
          </span>
        );
      })}
    </div>
  );
}

function MatchRow({
  match,
  isSelected,
  onClick,
}: {
  match: SpanMatch;
  isSelected: boolean;
  onClick: () => void;
}) {
  const span = match.spanA ?? match.spanB!;
  const displayName = span.name.replace(/^qontinui\./, "");
  const phase = inferPhase(span.name);
  const colors = PHASE_COLORS[phase];
  const statusStyle = STATUS_COLORS[match.status];

  return (
    <div
      className={`flex items-center h-8 px-3 gap-2 cursor-pointer border-b border-zinc-800/30 hover:bg-zinc-800/50 ${
        isSelected ? "bg-zinc-700/50" : ""
      } ${statusStyle.bg}`}
      onClick={onClick}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => { if (e.key === "Enter") onClick(); }}
    >
      {/* Status badge */}
      <span className={`text-[10px] px-1.5 rounded ${statusStyle.text} shrink-0 w-[80px] text-center`}>
        {statusStyle.label}
      </span>

      {/* Span name */}
      <span className="text-xs text-zinc-300 truncate flex-1">{displayName}</span>

      {/* Phase badge */}
      <span className={`text-[10px] px-1 rounded ${colors.badge} ${colors.text} shrink-0`}>
        {phase}
      </span>

      {/* Duration A */}
      <span className="text-[11px] text-zinc-500 w-[60px] text-right shrink-0">
        {match.spanA ? formatDuration(match.spanA.duration_ms ?? 0) : "—"}
      </span>

      {/* Arrow */}
      <span className="text-zinc-600 shrink-0">→</span>

      {/* Duration B */}
      <span className="text-[11px] text-zinc-500 w-[60px] text-right shrink-0">
        {match.spanB ? formatDuration(match.spanB.duration_ms ?? 0) : "—"}
      </span>

      {/* Delta */}
      <span className={`text-[11px] w-[70px] text-right shrink-0 ${statusStyle.text}`}>
        {match.durationDelta !== null
          ? `${match.durationDelta > 0 ? "+" : ""}${formatDuration(match.durationDelta)}`
          : "—"}
        {match.durationDeltaPct !== null && Math.abs(match.durationDeltaPct) > 1
          ? ` (${match.durationDeltaPct > 0 ? "+" : ""}${match.durationDeltaPct.toFixed(0)}%)`
          : ""}
      </span>
    </div>
  );
}

function ComparisonDetail({ match }: { match: SpanMatch }) {
  const spanA = match.spanA;
  const spanB = match.spanB;
  const durA = spanA?.duration_ms ?? 0;
  const durB = spanB?.duration_ms ?? 0;
  const maxDur = Math.max(durA, durB, 1);

  return (
    <div className="p-3 space-y-3 text-xs border-b border-zinc-700">
      {/* Duration comparison bar chart */}
      <div className="space-y-1">
        <div className="text-zinc-500 text-[10px]">Duration comparison</div>
        <div className="flex items-center gap-2">
          <span className="text-zinc-500 w-10 shrink-0">A:</span>
          <div className="flex-1 h-4 bg-zinc-800 rounded overflow-hidden">
            <div
              className="h-full bg-blue-500/60 rounded"
              style={{ width: `${(durA / maxDur) * 100}%` }}
            />
          </div>
          <span className="text-zinc-400 w-16 text-right">{formatDuration(durA)}</span>
        </div>
        <div className="flex items-center gap-2">
          <span className="text-zinc-500 w-10 shrink-0">B:</span>
          <div className="flex-1 h-4 bg-zinc-800 rounded overflow-hidden">
            <div
              className={`h-full rounded ${durB > durA ? "bg-red-500/60" : "bg-emerald-500/60"}`}
              style={{ width: `${(durB / maxDur) * 100}%` }}
            />
          </div>
          <span className="text-zinc-400 w-16 text-right">{formatDuration(durB)}</span>
        </div>
      </div>

      {/* Status change */}
      {match.status === "status_changed" && (
        <div className="space-y-1">
          <div className="text-zinc-500 text-[10px]">Status change</div>
          <div className="flex gap-2">
            <span className={spanA?.success ? "text-emerald-400" : "text-red-400"}>
              A: {spanA?.success ? "Success" : "Error"}
            </span>
            <span className="text-zinc-600">→</span>
            <span className={spanB?.success ? "text-emerald-400" : "text-red-400"}>
              B: {spanB?.success ? "Success" : "Error"}
            </span>
          </div>
          {spanB?.error && (
            <pre className="text-red-300/80 whitespace-pre-wrap break-words bg-red-900/20 rounded p-2 text-[10px]">
              {spanB.error}
            </pre>
          )}
        </div>
      )}

      {/* Attribute diff */}
      {spanA && spanB && (
        <div className="space-y-1">
          <div className="text-zinc-500 text-[10px]">Attribute changes</div>
          {(() => {
            const allKeys = new Set([
              ...Object.keys(spanA.attributes),
              ...Object.keys(spanB.attributes),
            ]);
            const diffs: { key: string; a: string; b: string }[] = [];
            for (const key of allKeys) {
              const a = JSON.stringify(spanA.attributes[key] ?? null);
              const b = JSON.stringify(spanB.attributes[key] ?? null);
              if (a !== b) diffs.push({ key, a, b });
            }
            if (diffs.length === 0) {
              return <div className="text-zinc-600">No attribute changes</div>;
            }
            return diffs.map((d) => (
              <div key={d.key} className="flex gap-1 text-[10px]">
                <span className="text-zinc-500">{d.key}:</span>
                <span className="text-red-400/70 line-through">{d.a}</span>
                <span className="text-zinc-600">→</span>
                <span className="text-emerald-400/70">{d.b}</span>
              </div>
            ));
          })()}
        </div>
      )}
    </div>
  );
}

function RunSelector({
  label,
  value,
  onChange,
  runs,
}: {
  label: string;
  value: string | null;
  onChange?: (id: string) => void;
  runs: { id: string; label: string }[];
}) {
  return (
    <label className="flex items-center gap-1.5 text-[11px]">
      <span className="text-zinc-500">{label}:</span>
      <select
        value={value ?? ""}
        onChange={(e) => onChange?.(e.target.value)}
        disabled={!onChange}
        className="px-2 py-0.5 bg-zinc-800 border border-zinc-700 rounded text-zinc-300 text-[11px] max-w-[260px] truncate focus:outline-hidden focus:border-zinc-500 disabled:opacity-60"
      >
        <option value="">Select a run...</option>
        {runs.map((r) => (
          <option key={r.id} value={r.id}>
            {r.label}
          </option>
        ))}
      </select>
    </label>
  );
}

export const TraceComparison: React.FC<TraceComparisonProps> = ({
  currentExecutionId,
  baselineExecutionId,
  onSelectRunA,
  onSelectRunB,
  availableRuns,
  height,
}) => {
  const [selectedMatch, setSelectedMatch] = useState<SpanMatch | null>(null);
  const [statusFilter, setStatusFilter] = useState<SpanMatch["status"] | "all">("all");

  const { matches, summary, isLoading, error } = useTraceComparisonData(
    baselineExecutionId,
    currentExecutionId,
  );

  const handleSelectMatch = useCallback((match: SpanMatch) => {
    setSelectedMatch((prev) =>
      prev && (prev.spanA?.span_id ?? prev.spanB?.span_id) === (match.spanA?.span_id ?? match.spanB?.span_id)
        ? null
        : match,
    );
  }, []);

  const filteredMatches = statusFilter === "all"
    ? matches
    : matches.filter((m) => m.status === statusFilter);

  // Run selector bar (always shown)
  const runSelectorBar = availableRuns && availableRuns.length > 0 ? (
    <div className="flex items-center gap-4 px-3 py-1.5 bg-zinc-900 border-b border-zinc-700">
      <RunSelector
        label="Run A (baseline)"
        value={baselineExecutionId}
        onChange={onSelectRunA}
        runs={availableRuns}
      />
      <RunSelector
        label="Run B (current)"
        value={currentExecutionId}
        onChange={onSelectRunB}
        runs={availableRuns}
      />
    </div>
  ) : null;

  if (!currentExecutionId || !baselineExecutionId) {
    return (
      <div className="flex flex-col bg-zinc-900 overflow-hidden" style={{ height }}>
        {runSelectorBar}
        <div className="flex items-center justify-center flex-1 text-zinc-500 text-sm">
          {availableRuns && availableRuns.length > 0
            ? "Select two runs above to compare their execution traces."
            : "No previous runs available for comparison. Run the workflow at least twice to enable comparison."}
        </div>
      </div>
    );
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-40 text-zinc-500 text-sm">
        Loading comparison data...
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex items-center justify-center h-40 text-red-400 text-sm">
        Failed to load comparison: {error}
      </div>
    );
  }

  if (matches.length === 0) {
    return (
      <div className="flex items-center justify-center h-40 text-zinc-500 text-sm">
        No spans to compare
      </div>
    );
  }

  return (
    <div className="flex flex-col bg-zinc-900 overflow-hidden" style={{ height }}>
      {/* Run selector dropdowns */}
      {runSelectorBar}

      {/* Summary bar */}
      <SummaryBar summary={summary} />

      {/* Regression banner */}
      <RegressionBanner regressions={summary.regressions} />

      {/* Status filter tabs */}
      <div className="flex items-center gap-1 px-3 py-1 border-b border-zinc-700 text-[10px]">
        {(["all", "slower", "faster", "added", "removed", "status_changed", "unchanged"] as const).map((s) => (
          <button
            key={s}
            onClick={() => setStatusFilter(s)}
            className={`px-2 py-0.5 rounded ${
              statusFilter === s
                ? "bg-zinc-700 text-zinc-200"
                : "text-zinc-500 hover:text-zinc-300"
            }`}
          >
            {s === "all" ? "All" : STATUS_COLORS[s].label}
            {s !== "all" && (
              <span className="ml-1 text-zinc-600">
                ({matches.filter((m) => m.status === s).length})
              </span>
            )}
          </button>
        ))}
      </div>

      {/* Match list + detail */}
      <div className="flex flex-1 min-h-0 overflow-hidden">
        <div className="flex-1 overflow-y-auto">
          {/* Header */}
          <div className="flex items-center h-6 px-3 gap-2 border-b border-zinc-700 bg-zinc-900/50 text-[10px] text-zinc-600 sticky top-0">
            <span className="w-[80px] shrink-0">Status</span>
            <span className="flex-1">Span</span>
            <span className="w-[60px] text-right shrink-0">Run A</span>
            <span className="w-4" />
            <span className="w-[60px] text-right shrink-0">Run B</span>
            <span className="w-[70px] text-right shrink-0">Delta</span>
          </div>

          {filteredMatches.map((match, i) => {
            const key = match.spanA?.span_id ?? match.spanB?.span_id ?? String(i);
            const isSelected =
              selectedMatch &&
              (selectedMatch.spanA?.span_id ?? selectedMatch.spanB?.span_id) ===
                (match.spanA?.span_id ?? match.spanB?.span_id);
            return (
              <React.Fragment key={key}>
                <MatchRow
                  match={match}
                  isSelected={!!isSelected}
                  onClick={() => handleSelectMatch(match)}
                />
                {isSelected && <ComparisonDetail match={match} />}
              </React.Fragment>
            );
          })}
        </div>
      </div>
    </div>
  );
};

/**
 * ScriptedOutputPanel.tsx
 *
 * LLM Analytics widget that renders per-task_run aggregates of the
 * `scripted_output.*` activity-timeline events emitted by the "think-in-
 * code" subsystem (see `step_output/script_emitter.rs` and
 * `lib/step-output-handlers/scripted-base.ts`).
 *
 * Source: `get_scripted_output_stats` Tauri command via
 * {@link useScriptedOutputStats}.
 *
 * ## States
 * - **Loading:** shows a neutral placeholder.
 * - **Empty** (zero attempts AND zero cache hits AND zero llm_ok): shows
 *   the "no scripted-output activity yet" hint.
 * - **Data:** shows a grid of metric tiles and a fallbacks-by-reason table.
 *
 * Phase B of the script-emitter-wiring plan may add emitter-side
 * `attempted` / `worker_ok` / `bytes_avoided` events; until then those
 * counters can be 0 and we render accordingly.
 */

import { Cpu, RefreshCw } from "lucide-react";
import { MetricCard } from "../performance-dashboard/MetricCard";
import { useScriptedOutputStats } from "../../hooks/useScriptedOutputStats";

interface ScriptedOutputPanelProps {
  /**
   * If provided, scope aggregates to a single task run. If omitted, the
   * panel shows a global view across all runs.
   */
  taskRunId?: string;
}

/** Format a byte count as B / KB / MB / GB. */
function humanizeBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let idx = 0;
  let v = n;
  while (v >= 1024 && idx < units.length - 1) {
    v /= 1024;
    idx += 1;
  }
  const fixed = v >= 100 || idx === 0 ? v.toFixed(0) : v.toFixed(1);
  return `${fixed} ${units[idx]}`;
}

/** Format a token count with K/M suffixes. */
function humanizeTokens(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return n.toLocaleString();
}

/**
 * Format a ratio numerator/denominator as a percent.
 * Returns "—" if the denominator is zero (avoids misleading 0% signals when
 * nothing's been attempted yet).
 */
function formatRate(numerator: number, denominator: number): string {
  if (denominator <= 0) return "—";
  const pct = Math.max(0, Math.min(100, (numerator / denominator) * 100));
  return `${pct.toFixed(pct >= 99.95 ? 0 : 1)}%`;
}

export function ScriptedOutputPanel({ taskRunId }: ScriptedOutputPanelProps) {
  const { stats, loading, error, refresh } = useScriptedOutputStats(taskRunId);

  const empty =
    !stats ||
    (stats.attempted === 0 &&
      stats.cacheHit === 0 &&
      stats.llmOk === 0 &&
      stats.workerOk === 0 &&
      Object.keys(stats.fallbacks).length === 0);

  // Partition fallbacks into emitter-path vs worker-path so the tiles can
  // use the right denominator for each metric. The split is by reason:
  // - Worker-path fallbacks ("bad_expression") fire after the emitter
  //   produced a script that then threw inside the node:vm sandbox. They
  //   count against `cacheHit + llmOk`, not against total emitter calls.
  // - Everything else (timeout, breaker_open, cost_cap, token_budget,
  //   disabled, emitter_error) is an emitter-path terminal state: the LLM
  //   path never produced a usable script.
  const WORKER_FALLBACK_REASONS = new Set(["bad_expression"]);
  const totalFallbacks = stats ? Object.values(stats.fallbacks).reduce((a, b) => a + b, 0) : 0;
  const workerFallbacks = stats
    ? Object.entries(stats.fallbacks)
        .filter(([reason]) => WORKER_FALLBACK_REASONS.has(reason))
        .reduce((a, [, c]) => a + c, 0)
    : 0;
  const emitterFallbacks = totalFallbacks - workerFallbacks;

  // Denominator for emitter-level metrics. `attempted` (from
  // `summarizeViaScript`) is authoritative when emitted, but we fall back
  // to the sum of terminal Phase-A counters and take the max so the
  // denominator is always ≥ the observed terminal events — otherwise
  // rates could exceed 100% when `attempted` is under-emitted (e.g.
  // direct `invoke()` calls in tests bypass the base handler).
  const emitterCalls = stats
    ? Math.max(stats.attempted, stats.cacheHit + stats.llmOk + emitterFallbacks)
    : 0;

  // Number of scripts actually sent to the worker (produced by cache or
  // LLM). Used as the denominator for Worker-OK and worker-fallback rates.
  const workerCalls = stats ? stats.cacheHit + stats.llmOk : 0;

  const fallbackRows = stats ? Object.entries(stats.fallbacks).sort((a, b) => b[1] - a[1]) : [];

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <div className="flex items-center justify-between mb-4">
        <h3 className="font-semibold flex items-center gap-2">
          <Cpu className="w-4 h-4" />
          Scripted Output (think-in-code)
          {taskRunId && (
            <span className="ml-2 text-xs font-normal text-muted-foreground font-mono">
              run {taskRunId.slice(0, 8)}
            </span>
          )}
        </h3>
        <button
          onClick={refresh}
          className="p-1.5 rounded-md hover:bg-muted transition-colors"
          title="Refresh scripted-output stats"
          aria-label="Refresh scripted-output stats"
        >
          <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {error && (
        <div className="text-sm text-red-500 py-4">
          Failed to load scripted-output stats: {error}
        </div>
      )}

      {loading && !stats && !error && (
        <div className="text-center text-muted-foreground py-8">Loading...</div>
      )}

      {!loading && !error && empty && (
        <div className="text-center text-muted-foreground py-8 text-sm">
          No scripted-output activity yet — run a handler with stdout &gt; 8 KB and{" "}
          <code className="font-mono">QONTINUI_SCRIPTED_OUTPUT=1</code>.
        </div>
      )}

      {!empty && stats && (
        <>
          <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
            <MetricCard
              title="Attempts"
              value={emitterCalls.toLocaleString()}
              subtitle={
                stats.attempted === 0 && emitterCalls > 0
                  ? "derived (Phase B pending)"
                  : "emitter calls"
              }
            />
            <MetricCard
              title="Cache hit rate"
              value={formatRate(stats.cacheHit, emitterCalls)}
              subtitle={`${stats.cacheHit.toLocaleString()} hits`}
              variant={
                emitterCalls > 0 && stats.cacheHit / emitterCalls >= 0.4 ? "success" : "default"
              }
            />
            <MetricCard
              title="LLM-OK rate"
              value={formatRate(stats.llmOk, emitterCalls)}
              subtitle={`${stats.llmOk.toLocaleString()} calls`}
            />
            <MetricCard
              title="Worker-OK rate"
              value={workerCalls > 0 ? formatRate(stats.workerOk, workerCalls) : "—"}
              subtitle={
                workerCalls === 0
                  ? "no scripts run yet"
                  : `${stats.workerOk.toLocaleString()} of ${workerCalls.toLocaleString()} scripts`
              }
            />
            <MetricCard
              title="Emitter fallback rate"
              value={formatRate(emitterFallbacks, emitterCalls)}
              subtitle={
                workerFallbacks > 0
                  ? `${emitterFallbacks.toLocaleString()} emitter + ${workerFallbacks.toLocaleString()} worker`
                  : `${emitterFallbacks.toLocaleString()} fallbacks`
              }
              variant={
                emitterCalls > 0 && emitterFallbacks / emitterCalls >= 0.15 ? "warning" : "default"
              }
            />
            <MetricCard title="Bytes avoided" value={humanizeBytes(stats.bytesAvoided)} />
            <MetricCard
              title="Tokens in"
              value={humanizeTokens(stats.totalTokensIn)}
              subtitle="across llm_ok"
            />
            <MetricCard
              title="Tokens out"
              value={humanizeTokens(stats.totalTokensOut)}
              subtitle="across llm_ok"
            />
          </div>

          {fallbackRows.length > 0 && (
            <div className="mt-4">
              <h4 className="text-sm font-medium text-muted-foreground mb-2">
                Fallbacks by reason
              </h4>
              <div className="overflow-x-auto max-h-48 overflow-y-auto">
                <table className="w-full text-sm">
                  <thead className="sticky top-0 bg-card">
                    <tr className="border-b border-border text-xs text-muted-foreground">
                      <th className="py-2 px-3 font-medium text-left">Reason</th>
                      <th className="py-2 px-3 font-medium text-right">Count</th>
                      <th className="py-2 px-3 font-medium text-right">Share</th>
                    </tr>
                  </thead>
                  <tbody>
                    {fallbackRows.map(([reason, count]) => (
                      <tr
                        key={reason}
                        className="border-b border-border last:border-0 hover:bg-muted/50"
                      >
                        <td className="py-2 px-3 font-mono text-xs">{reason}</td>
                        <td className="py-2 px-3 text-right tabular-nums">
                          {count.toLocaleString()}
                        </td>
                        <td className="py-2 px-3 text-right tabular-nums text-muted-foreground">
                          {totalFallbacks > 0
                            ? `${((count / totalFallbacks) * 100).toFixed(1)}%`
                            : "—"}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}
        </>
      )}
    </div>
  );
}

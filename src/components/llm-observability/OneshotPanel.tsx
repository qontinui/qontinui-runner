/**
 * OneshotPanel.tsx
 *
 * Sibling tile to {@link ScriptedOutputPanel} that surfaces process-local
 * counters for `OneshotLlm` adapter calls (Phase 0 of
 * `plans/productivity-stack-product-readiness.md`).
 *
 * The one-shot subsystem is what Phases 3/4/5 of the productivity stack
 * use to call an LLM with a typed JSON schema and get a typed Rust struct
 * back (`/decompose-plan`, `/auto-review`, `/rewind-session`,
 * `/summarize-session` once those land in Rust). This panel renders the
 * call counts + cache-hit ratios so users can see which adapter is
 * winning and whether prompt caching is firing.
 *
 * ## States
 * - **Loading:** neutral placeholder.
 * - **Empty** (zero calls AND zero errors): "no one-shot activity yet" hint.
 * - **Data:** grid of metric tiles + per-provider breakdown.
 *
 * Counters reset on runner restart.
 */

import { Bot, RefreshCw } from "lucide-react";
import { MetricCard } from "../performance-dashboard/MetricCard";
import { useOneshotStats } from "../../hooks/useOneshotStats";

/** Format a token count with K/M suffixes. */
function humanizeTokens(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return n.toLocaleString();
}

/**
 * Format a ratio numerator/denominator as a percent. Returns "—" if the
 * denominator is zero (avoids misleading 0% signals when nothing's been
 * attempted yet).
 */
function formatRate(numerator: number, denominator: number): string {
  if (denominator <= 0) return "—";
  const pct = Math.max(0, Math.min(100, (numerator / denominator) * 100));
  return `${pct.toFixed(pct >= 99.95 ? 0 : 1)}%`;
}

/** Sum the values of a counter map. */
function sumValues(map: Record<string, number>): number {
  return Object.values(map).reduce((a, b) => a + b, 0);
}

export function OneshotPanel() {
  const { stats, loading, error, refresh } = useOneshotStats();

  const totalCalls = stats ? sumValues(stats.callsTotal) : 0;
  const totalErrors = stats ? sumValues(stats.errorsTotal) : 0;
  const totalCacheHits = stats ? sumValues(stats.cacheHitsTotal) : 0;

  const empty = !stats || (totalCalls === 0 && totalErrors === 0);

  // Per-provider rows for the breakdown table. Sorted by call count desc,
  // tied providers fall back to alphabetical for stable ordering across
  // refreshes.
  const providerRows = stats
    ? Object.entries(stats.callsTotal)
        .map(([provider, calls]) => ({
          provider,
          calls,
          cacheHits: stats.cacheHitsTotal[provider] ?? 0,
        }))
        .sort((a, b) => b.calls - a.calls || a.provider.localeCompare(b.provider))
    : [];

  // Error rows for the breakdown table.
  const errorRows = stats
    ? Object.entries(stats.errorsTotal)
        .map(([kind, count]) => ({ kind, count }))
        .sort((a, b) => b.count - a.count || a.kind.localeCompare(b.kind))
    : [];

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <div className="flex items-center justify-between mb-4">
        <h3 className="font-semibold flex items-center gap-2">
          <Bot className="w-4 h-4" />
          One-shot LLM (productivity stack)
        </h3>
        <button
          onClick={refresh}
          className="p-1.5 rounded-md hover:bg-muted transition-colors"
          title="Refresh one-shot stats"
          aria-label="Refresh one-shot stats"
        >
          <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>

      {error && (
        <div className="text-sm text-red-500 py-4">Failed to load one-shot stats: {error}</div>
      )}

      {loading && !stats && !error && (
        <div className="text-center text-muted-foreground py-8">Loading...</div>
      )}

      {!loading && !error && empty && (
        <div className="text-center text-muted-foreground py-8 text-sm">
          No one-shot activity yet — fires when productivity-stack tasks (
          <code className="font-mono">/decompose-plan</code>,{" "}
          <code className="font-mono">/auto-review</code>, etc.) are run via the in-product Rust
          path.
        </div>
      )}

      {!empty && stats && (
        <>
          <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
            <MetricCard title="Calls" value={totalCalls.toLocaleString()} subtitle="successful" />
            <MetricCard
              title="Cache hits"
              value={totalCacheHits.toLocaleString()}
              subtitle={
                totalCalls > 0
                  ? `${formatRate(totalCacheHits, totalCalls)} of calls`
                  : "no calls yet"
              }
              variant={totalCalls > 0 && totalCacheHits / totalCalls >= 0.4 ? "success" : "default"}
            />
            <MetricCard
              title="Errors"
              value={totalErrors.toLocaleString()}
              subtitle={
                totalCalls + totalErrors > 0
                  ? `${formatRate(totalErrors, totalCalls + totalErrors)} of attempts`
                  : "no calls yet"
              }
              variant={
                totalCalls + totalErrors > 0 && totalErrors / (totalCalls + totalErrors) >= 0.15
                  ? "warning"
                  : "default"
              }
            />
            <MetricCard
              title="Cache-tokens read"
              value={humanizeTokens(stats.cacheReadTokens)}
              subtitle="billed at ~10% rate"
            />
            <MetricCard
              title="Tokens in"
              value={humanizeTokens(stats.totalTokensIn)}
              subtitle="across calls"
            />
            <MetricCard
              title="Tokens out"
              value={humanizeTokens(stats.totalTokensOut)}
              subtitle="across calls"
            />
            <MetricCard
              title="Cache-creation tokens"
              value={humanizeTokens(stats.cacheCreationTokens)}
              subtitle="cache writes"
            />
            <MetricCard
              title="Prompt cache hit"
              value={formatRate(
                stats.cacheReadTokens,
                stats.cacheReadTokens + stats.cacheCreationTokens,
              )}
              subtitle="read / (read + create)"
              variant={
                stats.cacheReadTokens + stats.cacheCreationTokens > 0 &&
                stats.cacheReadTokens / (stats.cacheReadTokens + stats.cacheCreationTokens) >= 0.7
                  ? "success"
                  : "default"
              }
            />
          </div>

          {providerRows.length > 0 && (
            <div className="mt-4">
              <h4 className="text-sm font-medium text-muted-foreground mb-2">Calls by provider</h4>
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b border-border text-xs text-muted-foreground">
                      <th className="py-2 px-3 font-medium text-left">Provider</th>
                      <th className="py-2 px-3 font-medium text-right">Calls</th>
                      <th className="py-2 px-3 font-medium text-right">Cache hits</th>
                      <th className="py-2 px-3 font-medium text-right">Hit rate</th>
                    </tr>
                  </thead>
                  <tbody>
                    {providerRows.map(({ provider, calls, cacheHits }) => (
                      <tr
                        key={provider}
                        className="border-b border-border last:border-0 hover:bg-muted/50"
                      >
                        <td className="py-2 px-3 font-mono text-xs">{provider}</td>
                        <td className="py-2 px-3 text-right tabular-nums">
                          {calls.toLocaleString()}
                        </td>
                        <td className="py-2 px-3 text-right tabular-nums">
                          {cacheHits.toLocaleString()}
                        </td>
                        <td className="py-2 px-3 text-right tabular-nums text-muted-foreground">
                          {formatRate(cacheHits, calls)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {errorRows.length > 0 && (
            <div className="mt-4">
              <h4 className="text-sm font-medium text-muted-foreground mb-2">Errors by kind</h4>
              <div className="overflow-x-auto max-h-48 overflow-y-auto">
                <table className="w-full text-sm">
                  <thead className="sticky top-0 bg-card">
                    <tr className="border-b border-border text-xs text-muted-foreground">
                      <th className="py-2 px-3 font-medium text-left">Kind</th>
                      <th className="py-2 px-3 font-medium text-right">Count</th>
                      <th className="py-2 px-3 font-medium text-right">Share</th>
                    </tr>
                  </thead>
                  <tbody>
                    {errorRows.map(({ kind, count }) => (
                      <tr
                        key={kind}
                        className="border-b border-border last:border-0 hover:bg-muted/50"
                      >
                        <td className="py-2 px-3 font-mono text-xs">{kind}</td>
                        <td className="py-2 px-3 text-right tabular-nums">
                          {count.toLocaleString()}
                        </td>
                        <td className="py-2 px-3 text-right tabular-nums text-muted-foreground">
                          {totalErrors > 0 ? `${((count / totalErrors) * 100).toFixed(1)}%` : "—"}
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

/**
 * The context / cost / 5-hour-headroom strip shared by `SessionCard` and
 * `CompactZoneCard` (plan
 * `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
 * Phase 7). All the honesty rules live in `metricCells`; this only paints
 * them: absent is "—", stale is greyed with its age, and the source and age
 * are always in the tooltip.
 */

import { memo, useEffect, useState } from "react";
import { metricCells, type MetricCell, type SessionMetrics } from "./agentMetrics";

/** How often the strip re-reads the clock so ages advance on an idle card. */
const AGE_TICK_MS = 30_000;

function useNow(enabled: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!enabled) return;
    const id = setInterval(() => setNow(Date.now()), AGE_TICK_MS);
    return () => clearInterval(id);
  }, [enabled]);
  return now;
}

function Cell({ name, label, cell }: { name: string; label: string; cell: MetricCell }) {
  const tone = cell.absent
    ? "text-[#414868]"
    : cell.stale
      ? "text-[#565f89] opacity-60"
      : "text-[#a9b1d6]";
  return (
    <span
      className={`inline-flex items-center gap-0.5 shrink-0 ${tone}`}
      title={cell.title}
      data-metric={name}
      data-metric-absent={cell.absent ? "true" : undefined}
      data-metric-stale={cell.stale ? "true" : undefined}
      data-metric-source={cell.source ?? undefined}
    >
      <span className="text-[#565f89]">{label}</span>
      <span className="font-mono">{cell.text}</span>
      {cell.stale && <span className="italic">{cell.age ? `· ${cell.age}` : "· age ?"}</span>}
    </span>
  );
}

function AgentMetricsStripInner({
  metrics,
  nowMs,
  className = "",
}: {
  metrics: SessionMetrics | null;
  /** Fixed clock for tests; otherwise the strip ticks on its own. */
  nowMs?: number;
  className?: string;
}) {
  const ticking = useNow(nowMs === undefined && metrics !== null);
  const cells = metricCells(metrics, nowMs ?? ticking);
  return (
    <div
      className={`flex items-center gap-2 text-[9px] leading-tight ${className}`}
      data-agent-metrics="true"
    >
      <Cell name="context" label="ctx" cell={cells.context} />
      <Cell name="cost" label="cost" cell={cells.cost} />
      <Cell name="headroom" label="5h" cell={cells.headroom} />
    </div>
  );
}

export const AgentMetricsStrip = memo(AgentMetricsStripInner);

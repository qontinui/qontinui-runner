/**
 * CostFeedTable.tsx
 *
 * Scrollable table of recent cost events, newest first, max 50 displayed.
 */

import { List } from "lucide-react";
import type { CostUpdateEvent } from "./types";

interface CostFeedTableProps {
  events: CostUpdateEvent[];
}

function formatTime(timestamp: number): string {
  try {
    return new Date(timestamp).toLocaleTimeString();
  } catch {
    return String(timestamp);
  }
}

function formatCost(usd: number): string {
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(2)}`;
}

function formatTokenCount(count: string | number): string {
  const n = typeof count === "string" ? parseInt(count, 10) : count;
  if (isNaN(n)) return String(count);
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return n.toLocaleString();
}

function formatCacheRate(rate: number): string {
  return `${(rate * 100).toFixed(0)}%`;
}

export function CostFeedTable({ events }: CostFeedTableProps) {
  const displayEvents = events.slice(0, 50);

  return (
    <div className="bg-card rounded-lg border border-border/50 p-4">
      <h3 className="text-sm font-semibold flex items-center gap-2 mb-3">
        <List className="w-4 h-4" />
        Recent Cost Events
        {events.length > 0 && (
          <span className="text-xs text-muted-foreground font-normal">
            ({Math.min(events.length, 50)} of {events.length})
          </span>
        )}
      </h3>

      {displayEvents.length === 0 ? (
        <div className="text-center text-muted-foreground py-6">
          <p className="text-sm">No cost events yet</p>
        </div>
      ) : (
        <div className="max-h-64 overflow-y-auto">
          <table className="w-full text-xs">
            <thead className="sticky top-0 bg-card">
              <tr className="border-b border-border text-muted-foreground">
                <th className="text-left py-2 px-2 font-medium">Time</th>
                <th className="text-left py-2 px-2 font-medium">Phase</th>
                <th className="text-right py-2 px-2 font-medium">Iter</th>
                <th className="text-right py-2 px-2 font-medium">Tokens</th>
                <th className="text-right py-2 px-2 font-medium">Cost</th>
                <th className="text-right py-2 px-2 font-medium">Cumulative</th>
                <th className="text-right py-2 px-2 font-medium">Cache</th>
              </tr>
            </thead>
            <tbody>
              {displayEvents.map((evt, i) => (
                <tr
                  key={`${evt.timestamp}-${i}`}
                  className="border-b border-border/30 hover:bg-muted/20 transition-colors"
                >
                  <td className="py-1.5 px-2 text-muted-foreground">{formatTime(evt.timestamp)}</td>
                  <td className="py-1.5 px-2">{evt.phase}</td>
                  <td className="py-1.5 px-2 text-right text-muted-foreground">
                    {evt.iteration ?? "-"}
                  </td>
                  <td className="py-1.5 px-2 text-right text-muted-foreground">
                    {formatTokenCount(evt.input_tokens)}/{formatTokenCount(evt.output_tokens)}
                  </td>
                  <td className="py-1.5 px-2 text-right font-mono">{formatCost(evt.cost_usd)}</td>
                  <td className="py-1.5 px-2 text-right font-mono">
                    {formatCost(evt.cumulative_cost_usd)}
                  </td>
                  <td className="py-1.5 px-2 text-right text-muted-foreground">
                    {formatCacheRate(evt.cache_hit_rate)}
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

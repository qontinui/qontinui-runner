/**
 * SummaryCards.tsx
 *
 * Summary metric cards for the LLM Observability Dashboard.
 * Shows total cost, total tokens, total calls, and average cost per call.
 */

import { DollarSign, Hash, Phone, TrendingUp } from "lucide-react";
import { MetricCard } from "../performance-dashboard/MetricCard";
import type { TokenUsageSummary } from "./types";

interface SummaryCardsProps {
  summary: TokenUsageSummary;
}

/** Format cents as dollars: $X.XX */
function formatCost(cents: number): string {
  return `$${(cents / 100).toFixed(2)}`;
}

/** Format large token counts with K/M suffixes */
function formatTokenCount(count: number): string {
  if (count >= 1_000_000) {
    return `${(count / 1_000_000).toFixed(1)}M`;
  }
  if (count >= 1_000) {
    return `${(count / 1_000).toFixed(1)}K`;
  }
  return count.toLocaleString();
}

export function SummaryCards({ summary }: SummaryCardsProps) {
  const totalTokens = summary.total_input_tokens + summary.total_output_tokens;

  return (
    <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
      <MetricCard
        title="Total Cost"
        value={formatCost(summary.total_cost_cents)}
        subtitle={`${summary.unique_providers} provider${summary.unique_providers !== 1 ? "s" : ""}`}
        icon={DollarSign}
      />
      <MetricCard
        title="Total Tokens"
        value={formatTokenCount(totalTokens)}
        subtitle={`${formatTokenCount(summary.total_input_tokens)} in / ${formatTokenCount(summary.total_output_tokens)} out`}
        icon={Hash}
      />
      <MetricCard
        title="Total Calls"
        value={summary.total_calls.toLocaleString()}
        subtitle={`${summary.unique_models} model${summary.unique_models !== 1 ? "s" : ""}`}
        icon={Phone}
      />
      <MetricCard
        title="Avg Cost/Call"
        value={formatCost(summary.avg_cost_per_call_cents)}
        subtitle={
          summary.avg_duration_ms != null
            ? `~${Math.round(summary.avg_duration_ms)}ms avg`
            : undefined
        }
        icon={TrendingUp}
      />
    </div>
  );
}

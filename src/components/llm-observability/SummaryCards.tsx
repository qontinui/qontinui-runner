/**
 * SummaryCards.tsx
 *
 * Summary metric cards for the LLM Observability Dashboard.
 * Shows total cost, total tokens, total calls, and average cost per call.
 */

import { DollarSign, Hash, Phone, TrendingUp } from "lucide-react";
import { useUIElement } from "@qontinui/ui-bridge";
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

  const { ref: totalCostRef } = useUIElement({
    id: "llm-analytics-metric-total-cost",
    type: "generic",
    label: "Total Cost metric card",
  });
  const { ref: totalTokensRef } = useUIElement({
    id: "llm-analytics-metric-total-tokens",
    type: "generic",
    label: "Total Tokens metric card",
  });
  const { ref: totalCallsRef } = useUIElement({
    id: "llm-analytics-metric-total-calls",
    type: "generic",
    label: "Total Calls metric card",
  });
  const { ref: avgCostPerCallRef } = useUIElement({
    id: "llm-analytics-metric-avg-cost-per-call",
    type: "generic",
    label: "Avg Cost per Call metric card",
  });

  return (
    <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
      <div ref={totalCostRef}>
        <MetricCard
          title="Total Cost"
          value={formatCost(summary.total_cost_cents)}
          subtitle={`${summary.unique_providers} provider${summary.unique_providers !== 1 ? "s" : ""}`}
          icon={DollarSign}
        />
      </div>
      <div ref={totalTokensRef}>
        <MetricCard
          title="Total Tokens"
          value={formatTokenCount(totalTokens)}
          subtitle={`${formatTokenCount(summary.total_input_tokens)} in / ${formatTokenCount(summary.total_output_tokens)} out`}
          icon={Hash}
        />
      </div>
      <div ref={totalCallsRef}>
        <MetricCard
          title="Total Calls"
          value={summary.total_calls.toLocaleString()}
          subtitle={`${summary.unique_models} model${summary.unique_models !== 1 ? "s" : ""}`}
          icon={Phone}
        />
      </div>
      <div ref={avgCostPerCallRef}>
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
    </div>
  );
}

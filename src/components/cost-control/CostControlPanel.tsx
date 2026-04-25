/**
 * CostControlPanel.tsx
 *
 * Main cost control dashboard panel with three sections:
 * - Budget & Cost: summary cards, phase breakdown
 * - Safety: circuit breaker status, anomaly feed
 * - Optimization: cost event feed, recommendations
 */

import { DollarSign, RefreshCw, AlertTriangle, Wallet } from "lucide-react";
import { MetricCard } from "../performance-dashboard/MetricCard";
import { useCostControl } from "../../hooks/useCostControl";
import { BudgetGauge } from "./BudgetGauge";
import { PhaseBreakdownChart } from "./PhaseBreakdownChart";
import { CostFeedTable } from "./CostFeedTable";
import { CircuitBreakerStatus } from "./CircuitBreakerStatus";
import { AnomalyFeed } from "./AnomalyFeed";
import { BudgetWarningBanner } from "./BudgetWarningBanner";
import { CostRecommendations } from "./CostRecommendations";

/** Source id → label used in the error banner. */
const SOURCE_LABELS: Record<string, string> = {
  "cost-dashboard": "Cost dashboard",
  budget: "Active budget",
};

/** Format USD: $X.XXXX for small, $X.XX for larger */
function formatCost(usd: number): string {
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(2)}`;
}

/** Format token counts with K/M suffixes */
function formatTokenCount(count: number): string {
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}K`;
  return count.toLocaleString();
}

export default function CostControlPanel() {
  const {
    costEvents,
    budgetWarnings,
    anomalies,
    dashboard,
    activeBudget,
    isLoading,
    status,
    refresh,
  } = useCostControl();

  // Render the page header unconditionally so navigation tests/snapshots
  // always see "Cost Control" even while data is loading or fetches fail.
  const header = (
    <div className="flex items-center justify-between">
      <div>
        <h2 className="text-lg font-semibold flex items-center gap-2">
          <DollarSign className="w-5 h-5" />
          Cost Control
        </h2>
        <p className="text-sm text-muted-foreground">
          Budget tracking, anomaly detection, and cost optimization
        </p>
      </div>
      <button
        onClick={refresh}
        className="p-2 rounded-md hover:bg-muted transition-colors"
        title="Refresh cost data"
        aria-label="Refresh cost data"
      >
        <RefreshCw className="w-4 h-4" />
      </button>
    </div>
  );

  const errorBanner = status.isError ? (
    <div
      role="alert"
      aria-live="polite"
      className="flex items-start gap-2 p-3 bg-destructive/10 border border-destructive/50 rounded-lg"
    >
      <AlertTriangle className="w-5 h-5 text-destructive shrink-0 mt-0.5" />
      <div className="flex-1 min-w-0 text-sm">
        <p className="font-medium text-destructive">Some cost data failed to load</p>
        <ul className="mt-1 space-y-0.5 text-destructive/80 text-xs">
          {status.errors.map((e) => (
            <li key={e.source}>
              <span className="font-medium">{SOURCE_LABELS[e.source] ?? e.source}:</span>{" "}
              {e.message}
            </li>
          ))}
        </ul>
      </div>
    </div>
  ) : null;

  // Initial load skeleton: keep heading visible and approximate the eventual
  // layout so the page doesn't "pop" when data arrives.
  if (isLoading && !dashboard && !activeBudget) {
    return (
      <div className="h-full overflow-auto p-4 space-y-4">
        {header}
        {errorBanner}
        <div className="flex items-center gap-2 text-sm text-muted-foreground" aria-live="polite">
          <RefreshCw className="w-4 h-4 animate-spin" />
          Loading cost data...
        </div>
        <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
          {Array.from({ length: 4 }).map((_, i) => (
            <div
              key={i}
              className="h-20 rounded-lg border border-border/50 bg-card animate-pulse"
            />
          ))}
        </div>
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
          <div className="h-56 rounded-lg border border-border/50 bg-card animate-pulse" />
          <div className="h-56 rounded-lg border border-border/50 bg-card animate-pulse" />
        </div>
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
          <div className="h-32 rounded-lg border border-border/50 bg-card animate-pulse" />
          <div className="h-32 rounded-lg border border-border/50 bg-card animate-pulse" />
        </div>
        <div className="h-40 rounded-lg border border-border/50 bg-card animate-pulse" />
      </div>
    );
  }

  return (
    <div className="h-full overflow-auto p-4 space-y-4">
      {header}

      {/* Fetch error banner */}
      {errorBanner}

      {/* Budget Warnings */}
      <BudgetWarningBanner warnings={budgetWarnings} />

      {/* ============================================================ */}
      {/* Section 1: Budget & Cost                                     */}
      {/* ============================================================ */}

      {/* Summary Cards */}
      <div data-tutorial-id="cc-summary-cards" className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <MetricCard
          title="Total Cost"
          value={dashboard ? formatCost(dashboard.total_cost_usd) : "$0.00"}
          subtitle="Last 30 days"
          icon={DollarSign}
        />
        <MetricCard
          title="Total Tokens"
          value={dashboard ? formatTokenCount(dashboard.total_tokens) : "0"}
          subtitle="Input + Output"
          icon={Wallet}
        />
        <MetricCard
          title="Cache Hit Rate"
          value={dashboard ? `${(dashboard.cache_hit_rate * 100).toFixed(0)}%` : "0%"}
          subtitle="Token cache efficiency"
        />
        <MetricCard
          title="Cache Savings"
          value={dashboard ? formatCost(dashboard.cache_savings_usd) : "$0.00"}
          subtitle="Estimated savings"
          variant="success"
        />
      </div>

      {/* Budget Gauge + Phase Breakdown */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        {/* Budget Gauge */}
        <div
          data-tutorial-id="cc-budget-gauge"
          className="bg-card rounded-lg border border-border/50 p-4"
        >
          <h3 className="text-sm font-semibold flex items-center gap-2 mb-4">
            <Wallet className="w-4 h-4" />
            Budget Utilization
          </h3>
          {activeBudget ? (
            <div className="flex justify-center">
              <BudgetGauge budget={activeBudget} />
            </div>
          ) : (
            <div className="text-center text-muted-foreground py-8">
              <AlertTriangle className="w-8 h-8 mx-auto mb-2 opacity-50" />
              <p className="text-sm">No active budget.</p>
              <p className="text-xs mt-1">Configure one in Settings &rarr; Cost Control.</p>
            </div>
          )}
        </div>

        {/* Phase Breakdown */}
        <PhaseBreakdownChart data={dashboard?.per_phase_breakdown ?? []} />
      </div>

      {/* ============================================================ */}
      {/* Section 2: Safety                                            */}
      {/* ============================================================ */}

      <div data-tutorial-id="cc-safety-section" className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <CircuitBreakerStatus budget={activeBudget} />
        <AnomalyFeed anomalies={anomalies} />
      </div>

      {/* ============================================================ */}
      {/* Section 3: Recent Activity                                   */}
      {/* ============================================================ */}

      <CostFeedTable events={costEvents} />

      {/* ============================================================ */}
      {/* Section 4: Optimization                                      */}
      {/* ============================================================ */}

      <CostRecommendations />
    </div>
  );
}

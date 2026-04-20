/**
 * LlmObservabilityDashboard.tsx
 *
 * Main dashboard for LLM token usage, cost analytics, and provider latency.
 * Fetches all data from Tauri backend commands and displays summary cards,
 * charts, and a task run cost table.
 */

import { useState } from "react";
import { CreditCard, RefreshCw, AlertTriangle, Clock } from "lucide-react";
import { useLlmAnalytics } from "../../hooks/useLlmAnalytics";
import type { LlmTimeRange } from "./types";
import { LLM_TIME_RANGE_LABELS } from "./types";
import { SummaryCards } from "./SummaryCards";
import { CostOverTimeChart } from "./CostOverTimeChart";
import { CostByModelChart } from "./CostByModelChart";
import { CostByPhaseChart } from "./CostByPhaseChart";
import { CostTrendChart } from "./CostTrendChart";
import { ProviderLatencyChart } from "./ProviderLatencyChart";
import { TaskRunCostTable } from "./TaskRunCostTable";
import { CostByTargetAppChart } from "./CostByTargetAppChart";
import { AccountUsageCard } from "./AccountUsageCard";
import { ScriptedOutputPanel } from "./ScriptedOutputPanel";

const TIME_RANGE_OPTIONS: LlmTimeRange[] = ["1d", "7d", "30d", "all"];

export default function LlmObservabilityDashboard() {
  const [timeRange, setTimeRange] = useState<LlmTimeRange>("7d");
  const {
    summary,
    dailyCost,
    costByModel,
    costByPhase,
    providerLatency,
    taskRunCosts,
    costByTargetApp,
    loading,
    error,
    refresh,
  } = useLlmAnalytics(timeRange);

  if (loading) {
    return (
      <div className="h-full flex items-center justify-center">
        <RefreshCw className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="h-full flex items-center justify-center text-muted-foreground">
        <div className="text-center">
          <AlertTriangle className="w-12 h-12 mx-auto mb-4 opacity-50" />
          <p>Failed to load LLM analytics</p>
          <p className="text-sm mt-2">{error}</p>
          <button
            onClick={refresh}
            className="mt-4 px-4 py-2 rounded-md bg-primary text-primary-foreground text-sm hover:bg-primary/90 transition-colors"
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (!summary || summary.total_calls === 0) {
    return (
      <div className="h-full overflow-auto p-4 space-y-6">
        <AccountUsageCard />
        <div className="flex items-center justify-center text-muted-foreground pt-4">
          <div className="text-center">
            <CreditCard className="w-12 h-12 mx-auto mb-4 opacity-50" />
            <p>No LLM usage data available</p>
            <p className="text-sm mt-2">Run some workflows to start collecting LLM analytics</p>
          </div>
        </div>
        {/* Scripted-output still renders in the zero-usage empty state: the
            think-in-code path can record fallback/attempted events even when
            no LLM call has succeeded yet. */}
        <ScriptedOutputPanel />
      </div>
    );
  }

  return (
    <div
      className="h-full overflow-auto p-4 space-y-4"
      data-tutorial-id="llm-observability-dashboard"
    >
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold flex items-center gap-2">
            <CreditCard className="w-5 h-5" />
            LLM Analytics
          </h2>
          <p className="text-sm text-muted-foreground">
            Token usage, costs, and provider performance
          </p>
        </div>
        <div className="flex items-center gap-3">
          {/* Time Range Selector */}
          <div className="flex items-center gap-2" data-tutorial-id="llm-time-range">
            <Clock className="w-4 h-4 text-muted-foreground" />
            <select
              value={timeRange}
              onChange={(e) => setTimeRange(e.target.value as LlmTimeRange)}
              aria-label="Select time range"
              className="bg-muted border border-border rounded-md px-3 py-1.5 text-sm focus:outline-hidden focus:ring-2 focus:ring-primary"
            >
              {TIME_RANGE_OPTIONS.map((option) => (
                <option key={option} value={option}>
                  {LLM_TIME_RANGE_LABELS[option]}
                </option>
              ))}
            </select>
          </div>
          <button
            onClick={refresh}
            className="p-2 rounded-md hover:bg-muted transition-colors"
            title="Refresh analytics"
            aria-label="Refresh analytics"
          >
            <RefreshCw className="w-4 h-4" />
          </button>
        </div>
      </div>

      {/* Summary Cards */}
      <div data-tutorial-id="llm-summary-cards">
        <SummaryCards summary={summary} />
      </div>

      {/* Account Usage vs Expected */}
      <AccountUsageCard />

      {/* Charts Row 1 */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <CostOverTimeChart data={dailyCost} />
        <CostByModelChart data={costByModel} />
      </div>

      {/* Cost Trend (fetches its own data from the web backend) */}
      <CostTrendChart />

      {/* Scripted-output (think-in-code) aggregates — Phase C of script-emitter-wiring */}
      <ScriptedOutputPanel />

      {/* Charts Row 2 */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <CostByPhaseChart data={costByPhase} />
        <ProviderLatencyChart data={providerLatency} />
      </div>

      {/* UI Bridge Cost Attribution */}
      {costByTargetApp.length > 0 && <CostByTargetAppChart data={costByTargetApp} />}

      {/* Task Run Cost Table */}
      <TaskRunCostTable data={taskRunCosts} />
    </div>
  );
}

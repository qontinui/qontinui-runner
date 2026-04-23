/**
 * LlmObservabilityDashboard.tsx
 *
 * Main dashboard for LLM token usage, cost analytics, and provider latency.
 * Fetches all data from Tauri backend commands and displays summary cards,
 * charts, and a task run cost table.
 */

import { useState, type ReactElement } from "react";
import { useUIComponent, useUIElement } from "@qontinui/ui-bridge";
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
import { EmitterProviderControl } from "./EmitterProviderControl";

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

  // --- UI Bridge registrations ---
  // Page marker — surfaces in currentRouteOnly snapshots when the
  // LLM Analytics tab is active so callers can see this page exists
  // beyond the sidebar buttons.
  useUIComponent({
    id: "page:llm-analytics",
    name: "LLM Analytics page",
    description:
      "LLM token usage, cost analytics, and provider latency dashboard for the active task runs",
  });
  const { ref: pageRootRef } = useUIElement({
    id: "llm-analytics-root",
    type: "generic",
    label: "LLM Analytics page root",
  });
  const { ref: refreshButtonRef } = useUIElement({
    id: "llm-analytics-refresh",
    type: "button",
    label: "Refresh LLM analytics",
    actions: ["click"],
  });
  const { ref: timeRangeSelectRef } = useUIElement({
    id: "llm-analytics-time-range",
    type: "select",
    label: "Time range selector",
  });
  const { ref: costOverTimeRef } = useUIElement({
    id: "llm-analytics-chart-cost-over-time",
    type: "generic",
    label: "Cost over time chart",
  });
  const { ref: costByModelRef } = useUIElement({
    id: "llm-analytics-chart-cost-by-model",
    type: "generic",
    label: "Cost by model chart",
  });
  const { ref: costByPhaseRef } = useUIElement({
    id: "llm-analytics-chart-cost-by-phase",
    type: "generic",
    label: "Cost by phase chart",
  });
  const { ref: providerLatencyRef } = useUIElement({
    id: "llm-analytics-chart-provider-latency",
    type: "generic",
    label: "Provider latency chart",
  });
  const { ref: costTrendRef } = useUIElement({
    id: "llm-analytics-chart-cost-trend",
    type: "generic",
    label: "Cost trend chart",
  });
  const { ref: costByTargetAppRef } = useUIElement({
    id: "llm-analytics-chart-cost-by-target-app",
    type: "generic",
    label: "Cost by target app chart",
  });
  const { ref: taskRunCostTableRef } = useUIElement({
    id: "llm-analytics-task-run-cost-table",
    type: "generic",
    label: "Task run cost table",
  });

  // Always-on header: refresh button + time-range selector register and
  // render regardless of branch, so UI Bridge snapshots can verify both
  // registrations even when there is no data yet.
  const header = (
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
        <div
          ref={timeRangeSelectRef}
          className="flex items-center gap-2"
          data-tutorial-id="llm-time-range"
        >
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
          ref={refreshButtonRef}
          onClick={refresh}
          className="p-2 rounded-md hover:bg-muted transition-colors"
          title="Refresh analytics"
          aria-label="Refresh analytics"
        >
          <RefreshCw className="w-4 h-4" />
        </button>
      </div>
    </div>
  );

  let main: ReactElement;
  if (loading) {
    main = (
      <div className="flex items-center justify-center py-12">
        <RefreshCw className="w-6 h-6 animate-spin text-muted-foreground" />
      </div>
    );
  } else if (error) {
    main = (
      <div className="flex items-center justify-center text-muted-foreground py-12">
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
  } else if (!summary || summary.total_calls === 0) {
    main = (
      <div className="space-y-6">
        <AccountUsageCard />
        <div className="flex items-center justify-center text-muted-foreground pt-4">
          <div className="text-center">
            <CreditCard className="w-12 h-12 mx-auto mb-4 opacity-50" />
            <p>No LLM usage data available</p>
            <p className="text-sm mt-2">Run some workflows to start collecting LLM analytics</p>
          </div>
        </div>
        {/* Emitter provider control is always visible so fresh installs
            can switch to local Gemma before any emit has fired. Rendered
            above the panel in both zero-state and data branches. */}
        <EmitterProviderControl />
        {/* Scripted-output still renders in the zero-usage empty state: the
            think-in-code path can record fallback/attempted events even when
            no LLM call has succeeded yet. */}
        <ScriptedOutputPanel />
      </div>
    );
  } else {
    main = (
      <div className="space-y-4" data-tutorial-id="llm-observability-dashboard">
        {/* Summary Cards */}
        <div data-tutorial-id="llm-summary-cards">
          <SummaryCards summary={summary} />
        </div>

        {/* Account Usage vs Expected */}
        <AccountUsageCard />

        {/* Charts Row 1 */}
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
          <div ref={costOverTimeRef}>
            <CostOverTimeChart data={dailyCost} />
          </div>
          <div ref={costByModelRef}>
            <CostByModelChart data={costByModel} />
          </div>
        </div>

        {/* Cost Trend (fetches its own data from the web backend) */}
        <div ref={costTrendRef}>
          <CostTrendChart />
        </div>

        {/* Emitter provider control — lets users switch between Claude and
            local Gemma without hand-editing settings.json. */}
        <EmitterProviderControl />
        {/* Scripted-output (think-in-code) aggregates — Phase C of script-emitter-wiring */}
        <ScriptedOutputPanel />

        {/* Charts Row 2 */}
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
          <div ref={costByPhaseRef}>
            <CostByPhaseChart data={costByPhase} />
          </div>
          <div ref={providerLatencyRef}>
            <ProviderLatencyChart data={providerLatency} />
          </div>
        </div>

        {/* UI Bridge Cost Attribution */}
        {costByTargetApp.length > 0 && (
          <div ref={costByTargetAppRef}>
            <CostByTargetAppChart data={costByTargetApp} />
          </div>
        )}

        {/* Task Run Cost Table */}
        <div ref={taskRunCostTableRef}>
          <TaskRunCostTable data={taskRunCosts} />
        </div>
      </div>
    );
  }

  return (
    <div
      ref={pageRootRef}
      data-nav-item="page:llm-analytics"
      className="h-full overflow-auto p-4 space-y-4"
    >
      <header>{header}</header>
      <main>{main}</main>
    </div>
  );
}

/**
 * AutomationHealthDashboard.tsx
 *
 * Top-level dashboard composing all UI Bridge analytics panels.
 * Shows health score, recommendations, and detailed analytics.
 */

import { useState } from "react";
import { BarChart3, RefreshCw } from "lucide-react";
import { useGraphAnalyticsRefresh } from "../../hooks/useGraphAnalytics";
import { HealthScoreCard } from "./HealthScoreCard";
import { RecommendationsPanel } from "./RecommendationsPanel";
import { ElementInteractionHeatmap } from "./ElementInteractionHeatmap";
import { ActionBaselineTable } from "./ActionBaselineTable";
import { FailureTaxonomyPanel } from "./FailureTaxonomyPanel";
import { AutomationRegressionTable } from "./AutomationRegressionTable";
import { StallFrequencyChart } from "./StallFrequencyChart";
import { InterventionEffectivenessChart } from "./InterventionEffectivenessChart";
import { AnnotationGapTable } from "./AnnotationGapTable";

type TimeRange = 1 | 7 | 30;

const TIME_LABELS: Record<TimeRange, string> = {
  1: "24h",
  7: "7d",
  30: "30d",
};

export function AutomationHealthDashboard() {
  const [days, setDays] = useState<TimeRange>(7);
  const refresh = useGraphAnalyticsRefresh();

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold flex items-center gap-2">
          <BarChart3 className="w-5 h-5" />
          UI Bridge Automation Health
        </h2>
        <div className="flex items-center gap-2">
          {(Object.entries(TIME_LABELS) as [string, string][]).map(([d, label]) => (
            <button
              key={d}
              onClick={() => setDays(Number(d) as TimeRange)}
              className={`px-3 py-1 text-sm rounded-md ${
                days === Number(d)
                  ? "bg-primary text-primary-foreground"
                  : "bg-muted text-muted-foreground hover:bg-muted/80"
              }`}
            >
              {label}
            </button>
          ))}
          <button onClick={refresh} className="p-2 rounded-md bg-muted hover:bg-muted/80">
            <RefreshCw className="w-4 h-4" />
          </button>
        </div>
      </div>

      {/* Row 1: Health score + Recommendations */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <HealthScoreCard days={days} />
        <RecommendationsPanel days={days} />
      </div>

      {/* Row 2: Element reliability + Action baselines */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <ElementInteractionHeatmap />
        <ActionBaselineTable days={days} />
      </div>

      {/* Row 3: Failures + Regressions */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <FailureTaxonomyPanel days={days} />
        <AutomationRegressionTable days={days} />
      </div>

      {/* Row 4: Stalls + Interventions */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <StallFrequencyChart days={days} />
        <InterventionEffectivenessChart days={days} />
      </div>

      {/* Row 5: Annotation gaps */}
      <AnnotationGapTable />
    </div>
  );
}

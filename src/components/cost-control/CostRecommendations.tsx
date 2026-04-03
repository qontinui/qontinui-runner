/**
 * CostRecommendations.tsx
 *
 * Displays cost-optimization recommendations from the meta-optimizer system,
 * plus actionable insights derived from the current CostDashboard data
 * (e.g. low cache hit rate, high per-phase spend).
 */

import { Lightbulb, TrendingDown, CheckCircle2 } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import type { CostDashboard } from "./types";

/** Subset of the meta-optimizer Recommendation type relevant to cost display. */
interface Recommendation {
  id: string;
  optimizer_type: string;
  recommendation_type: string;
  target_agent: string | null;
  title: string;
  description: string;
  confidence: number;
  status: string;
  created_at: string;
}

/** A locally-derived insight based on dashboard metrics. */
interface CostInsight {
  id: string;
  title: string;
  description: string;
  severity: "info" | "warning" | "critical";
}

const CONFIDENCE_COLORS: Record<string, string> = {
  high: "text-green-400 bg-green-900/30",
  medium: "text-yellow-400 bg-yellow-900/30",
  low: "text-zinc-400 bg-zinc-800/50",
};

function confidenceTier(confidence: number): "high" | "medium" | "low" {
  if (confidence >= 0.7) return "high";
  if (confidence >= 0.4) return "medium";
  return "low";
}

const SEVERITY_STYLES: Record<string, string> = {
  info: "border-blue-500/20 bg-blue-500/5",
  warning: "border-yellow-500/20 bg-yellow-500/5",
  critical: "border-red-500/20 bg-red-500/5",
};

const SEVERITY_DOT: Record<string, string> = {
  info: "bg-blue-400",
  warning: "bg-yellow-400",
  critical: "bg-red-400",
};

const STATUS_BADGE: Record<string, string> = {
  pending: "text-yellow-400 bg-yellow-900/30",
  applied: "text-green-400 bg-green-900/30",
  rejected: "text-zinc-500 bg-zinc-800/50",
  rolled_back: "text-orange-400 bg-orange-900/30",
};

/** Derive actionable insights from dashboard metrics. */
function deriveInsights(dashboard: CostDashboard | null): CostInsight[] {
  if (!dashboard) return [];
  const insights: CostInsight[] = [];

  // Low cache hit rate
  if (dashboard.cache_hit_rate < 0.15) {
    insights.push({
      id: "cache-critical",
      title: "Cache hit rate is critically low",
      description: `Only ${(dashboard.cache_hit_rate * 100).toFixed(0)}% of tokens are cache hits. Consider restructuring prompts to maximize prefix caching — place stable system instructions at the start, variable content at the end.`,
      severity: "critical",
    });
  } else if (dashboard.cache_hit_rate < 0.3) {
    insights.push({
      id: "cache-low",
      title: "Cache hit rate could be improved",
      description: `Cache hit rate is ${(dashboard.cache_hit_rate * 100).toFixed(0)}%. Reordering prompt sections so static content comes first can improve cache reuse and reduce costs.`,
      severity: "warning",
    });
  }

  // High budget utilization
  if (dashboard.budget_utilization != null && dashboard.budget_utilization > 0.85) {
    insights.push({
      id: "budget-high",
      title: "Approaching budget limit",
      description: `Budget utilization is at ${(dashboard.budget_utilization * 100).toFixed(0)}%. Consider reviewing expensive phases or enabling model downgrade for non-critical steps.`,
      severity: dashboard.budget_utilization > 0.95 ? "critical" : "warning",
    });
  }

  // Identify the most expensive phase
  if (dashboard.per_phase_breakdown.length > 1) {
    const sorted = [...dashboard.per_phase_breakdown].sort(
      (a, b) => b.cost_usd - a.cost_usd
    );
    const top = sorted[0];
    const totalCost = dashboard.total_cost_usd;
    if (totalCost > 0) {
      const fraction = top.cost_usd / totalCost;
      if (fraction > 0.6) {
        insights.push({
          id: "phase-dominant",
          title: `"${top.phase}" phase dominates spending`,
          description: `The ${top.phase} phase accounts for ${(fraction * 100).toFixed(0)}% of total cost ($${top.cost_usd.toFixed(4)}). Optimizing prompts or using a cheaper model for this phase could yield significant savings.`,
          severity: "info",
        });
      }
    }
  }

  // Good cache savings callout
  if (dashboard.cache_savings_usd > 0.5 && dashboard.cache_hit_rate >= 0.3) {
    insights.push({
      id: "cache-savings-good",
      title: "Cache savings are working well",
      description: `You've saved $${dashboard.cache_savings_usd.toFixed(2)} through prompt caching with a ${(dashboard.cache_hit_rate * 100).toFixed(0)}% hit rate. Keep prompt structures stable to maintain these savings.`,
      severity: "info",
    });
  }

  return insights;
}

export function CostRecommendations() {
  // Fetch meta-optimizer recommendations filtered to cost optimization
  const {
    data: recommendations,
    isLoading: recsLoading,
  } = useQuery<Recommendation[]>({
    queryKey: ["cost-recommendations"],
    queryFn: async () => {
      try {
        return await invoke<Recommendation[]>(
          "get_meta_optimizer_recommendations",
          { optimizerType: "pipeline_prompt", status: null }
        );
      } catch {
        // Command may not be available if PG isn't connected
        return [];
      }
    },
    refetchInterval: 30_000,
  });

  // Fetch dashboard for deriving insights
  const { data: dashboard } = useQuery<CostDashboard>({
    queryKey: ["cost-dashboard"],
    queryFn: () => invoke<CostDashboard>("get_cost_dashboard", { days: 30 }),
    refetchInterval: 60_000,
  });

  const costRecs = (recommendations ?? []).filter(
    (r) =>
      r.recommendation_type === "cost_reduction" ||
      r.recommendation_type === "model_downgrade" ||
      r.recommendation_type === "cache_optimization" ||
      r.recommendation_type === "duplicate_prompt" ||
      r.title.toLowerCase().includes("cost") ||
      r.title.toLowerCase().includes("cache") ||
      r.title.toLowerCase().includes("duplicate")
  );

  const insights = deriveInsights(dashboard ?? null);
  const hasContent = costRecs.length > 0 || insights.length > 0;

  return (
    <div className="bg-card rounded-lg border border-border/50 p-4">
      <h3 className="text-sm font-semibold flex items-center gap-2 mb-3">
        <Lightbulb className="w-4 h-4" />
        Optimization Recommendations
        {hasContent && (
          <span className="text-xs text-muted-foreground font-normal">
            ({costRecs.length + insights.length})
          </span>
        )}
      </h3>

      {recsLoading ? (
        <div className="text-sm text-muted-foreground py-4 text-center">
          Loading recommendations...
        </div>
      ) : !hasContent ? (
        <div className="text-center text-muted-foreground py-6">
          <TrendingDown className="w-8 h-8 mx-auto mb-2 opacity-50" />
          <p className="text-sm">No cost optimization recommendations yet</p>
          <p className="text-xs mt-1">
            Run workflows to accumulate data for cost analysis
          </p>
        </div>
      ) : (
        <div className="space-y-2 max-h-64 overflow-y-auto">
          {/* Meta-optimizer recommendations */}
          {costRecs.map((rec) => {
            const tier = confidenceTier(rec.confidence);
            return (
              <div
                key={rec.id}
                className="rounded-md border border-border/30 bg-muted/30 p-3"
              >
                <div className="flex items-center gap-2 mb-1">
                  <CheckCircle2 className="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
                  <span className="text-sm font-medium flex-1">
                    {rec.title}
                  </span>
                  <span
                    className={`text-[10px] px-1.5 py-0.5 rounded ${CONFIDENCE_COLORS[tier]}`}
                  >
                    {(rec.confidence * 100).toFixed(0)}%
                  </span>
                  <span
                    className={`text-[10px] px-1.5 py-0.5 rounded ${STATUS_BADGE[rec.status] || "text-zinc-400 bg-zinc-800/50"}`}
                  >
                    {rec.status}
                  </span>
                </div>
                <p className="text-xs text-muted-foreground ml-5.5 pl-px">
                  {rec.description}
                </p>
                {rec.target_agent && (
                  <span className="text-[10px] text-muted-foreground ml-5.5 pl-px mt-1 inline-block">
                    Agent: {rec.target_agent}
                  </span>
                )}
              </div>
            );
          })}

          {/* Dashboard-derived insights */}
          {insights.map((insight) => (
            <div
              key={insight.id}
              className={`rounded-md border p-3 ${SEVERITY_STYLES[insight.severity]}`}
            >
              <div className="flex items-center gap-2 mb-1">
                <span
                  className={`w-2 h-2 rounded-full flex-shrink-0 ${SEVERITY_DOT[insight.severity]}`}
                />
                <span className="text-sm font-medium">{insight.title}</span>
              </div>
              <p className="text-xs text-muted-foreground ml-4">
                {insight.description}
              </p>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

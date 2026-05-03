/**
 * PlanRecommendations — Phase 5 of the productivity-stack plan.
 *
 * "Coordinator suggests next plan" card. Renders inside the
 * CoordinatorDashboard above the Recommendations panel. Lists up to 3
 * candidate plans with a server-generated cheap-rule rationale and an
 * "Open plan" button that switches to the Productivity tab → Plans
 * sub-view with the plan pre-selected.
 *
 * The ranking comes from the cheap-rule endpoint
 * `GET /productivity/plan-recommendations` (no LLM). The LLM only
 * consults inside the `/coordinate` loop itself; this card stays cheap
 * so the dashboard doesn't burn tokens on every refresh.
 */

import { useCallback, useEffect, useState } from "react";
import { ListChecks, RefreshCw, ArrowRight, Loader2, Sparkles } from "lucide-react";
import { getPlanRecommendations, type PlanRecommendation } from "./reflectionApi";

export function PlanRecommendations() {
  const [rows, setRows] = useState<PlanRecommendation[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await getPlanRecommendations();
      setRows(data);
    } catch (err) {
      setRows([]);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async data load on dependency change
    void load();
  }, [load]);

  const openPlan = useCallback((planId: string) => {
    // The ProductivityPage listens for both events.
    window.dispatchEvent(new CustomEvent("productivity-set-view", { detail: { view: "plans" } }));
    window.dispatchEvent(
      new CustomEvent("productivity-select-plan", {
        detail: { planId },
      }),
    );
  }, []);

  return (
    <section
      role="region"
      aria-labelledby="productivity-plan-recommendations-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.plan-recommendations"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Sparkles className="w-4 h-4 text-purple-400" />
          <h2
            id="productivity-plan-recommendations-heading"
            className="text-sm font-semibold text-foreground"
          >
            Coordinator suggests next plan
          </h2>
          <span className="text-xs text-muted-foreground">top {rows.length}</span>
        </div>
        <button
          type="button"
          onClick={load}
          disabled={loading}
          className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 disabled:opacity-50"
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </header>

      {error ? (
        <div className="rounded-md border border-red-500/30 bg-red-500/10 p-3 text-xs text-red-400">
          {error}
        </div>
      ) : loading && rows.length === 0 ? (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 className="w-3 h-3 animate-spin" /> Computing recommendations…
        </div>
      ) : rows.length === 0 ? (
        <div className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground">
          No plan candidates with ready/unassigned tasks. Run
          <code className="font-mono mx-1">/decompose-plan</code>
          on a vetted plan to populate this card.
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {rows.map((row) => (
            <li
              key={row.planId}
              className="rounded-md border border-border/40 bg-background/40 p-3"
              data-ui-bridge-id="productivity.plan-recommendation-card"
              data-plan-id={row.planId}
            >
              <div className="flex items-start justify-between gap-3">
                <div className="flex flex-col gap-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <ListChecks className="w-3.5 h-3.5 text-muted-foreground" />
                    <span className="text-sm font-medium text-foreground truncate">
                      {row.title ?? row.planId}
                    </span>
                  </div>
                  <p className="text-xs text-muted-foreground">{row.reasoning}</p>
                </div>
                <button
                  type="button"
                  onClick={() => openPlan(row.planId)}
                  className="inline-flex items-center gap-1 rounded-md border border-border bg-muted/20 px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/40"
                >
                  Open plan
                  <ArrowRight className="w-3 h-3" />
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export default PlanRecommendations;

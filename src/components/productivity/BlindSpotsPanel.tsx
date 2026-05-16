/**
 * BlindSpotsPanel — Blind-Spot Recommender frontend.
 *
 * "Recommended instrumentation" card. Renders inside the
 * CoordinatorDashboard directly below the PlanRecommendations panel. Lists
 * ranked blind spots (observability regions the framework can't see but
 * that demand observation) with a server-generated recommendation and an
 * "Open" button that switches to the Productivity tab → Plans sub-view and
 * dispatches a `productivity-select-blind-spot` event so a future surface
 * can focus the affected region.
 *
 * The ranking comes from `GET /blind-spots` (cheap-rule, no LLM) — the same
 * cheap-card discipline as PlanRecommendations so the dashboard refresh
 * stays free.
 */

import { useCallback, useEffect, useState } from "react";
import { EyeOff, RefreshCw, ArrowRight, Loader2, ScanSearch } from "lucide-react";
import {
  getBlindSpots,
  type RecommendationKind,
  type ScoredBlindSpot,
  type SubSpace,
} from "./blindSpotsApi";

/** Human-readable label per sub-space — keeps the card legible without
 *  leaking the serde enum spelling into the UI. */
const SUB_SPACE_LABEL: Record<SubSpace, string> = {
  dom: "DOM",
  fsPath: "Filesystem path",
  fsContent: "Filesystem content",
  gitRef: "Git ref",
  gitCommit: "Git commit",
  procInstance: "Process instance",
  coord: "Coordinator",
};

/** Badge tint per recommendation kind. Mirrors the muted/amber/emerald
 *  palette the sibling panels use for status pills. */
const KIND_BADGE_CLASS: Record<RecommendationKind, string> = {
  "reactivate-observer": "border-emerald-500/40 bg-emerald-500/10 text-emerald-300",
  "extend-scope": "border-amber-500/40 bg-amber-500/10 text-amber-300",
  "deploy-observer": "border-purple-500/40 bg-purple-500/10 text-purple-300",
};

/** Cap the unblocks chip list so a wide region doesn't blow out the card. */
const MAX_UNBLOCKS_SHOWN = 4;

export function BlindSpotsPanel() {
  const [rows, setRows] = useState<ScoredBlindSpot[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await getBlindSpots();
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

  const openBlindSpot = useCallback((subSpace: SubSpace) => {
    // The ProductivityPage listens for `productivity-set-view`; a future
    // region-focus surface listens for `productivity-select-blind-spot`.
    window.dispatchEvent(new CustomEvent("productivity-set-view", { detail: { view: "plans" } }));
    window.dispatchEvent(
      new CustomEvent("productivity-select-blind-spot", {
        detail: { subSpace },
      }),
    );
  }, []);

  return (
    <section
      role="region"
      aria-labelledby="productivity-blind-spots-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.blind-spots"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <EyeOff className="w-4 h-4 text-amber-400" />
          <h2
            id="productivity-blind-spots-heading"
            className="text-sm font-semibold text-foreground"
          >
            Blind spots — recommended instrumentation
          </h2>
          <span className="text-xs text-muted-foreground">top {rows.length}</span>
        </div>
        <button
          type="button"
          onClick={load}
          disabled={loading}
          data-ui-bridge-id="productivity.blind-spots-refresh"
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
          <Loader2 className="w-3 h-3 animate-spin" /> Scoring blind spots…
        </div>
      ) : rows.length === 0 ? (
        <div className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground">
          No blind spots detected. The framework can observe everything currently in demand.
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {rows.map((row, idx) => {
            const unblocks = row.unblocks.slice(0, MAX_UNBLOCKS_SHOWN);
            const overflow = row.unblocks.length - unblocks.length;
            return (
              <li
                key={`${row.region.subSpace}-${idx}`}
                className="rounded-md border border-border/40 bg-background/40 p-3"
                data-ui-bridge-id="productivity.blind-spot-card"
                data-sub-space={row.region.subSpace}
              >
                <div className="flex items-start justify-between gap-3">
                  <div className="flex flex-col gap-1 min-w-0">
                    <div className="flex items-center gap-2 flex-wrap">
                      <ScanSearch className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                      <span className="text-sm font-medium text-foreground">
                        {SUB_SPACE_LABEL[row.region.subSpace]}
                      </span>
                      <span
                        className={`rounded-full border px-2 py-0.5 text-[10px] font-medium ${KIND_BADGE_CLASS[row.recommendation.kind]}`}
                      >
                        {row.recommendation.kind}
                      </span>
                      <span className="text-xs text-muted-foreground">
                        score {row.score.toFixed(1)}
                      </span>
                    </div>
                    <p className="text-xs text-muted-foreground">{row.recommendation.detail}</p>
                    {row.unblocks.length > 0 && (
                      <div className="flex items-center gap-1 flex-wrap text-[11px] text-muted-foreground">
                        <span className="text-foreground/70">unblocks:</span>
                        {unblocks.map((u) => (
                          <span
                            key={u}
                            className="rounded border border-border/60 bg-muted/20 px-1.5 py-0.5 font-mono truncate max-w-[16rem]"
                          >
                            {u}
                          </span>
                        ))}
                        {overflow > 0 && (
                          <span className="text-muted-foreground">+{overflow} more</span>
                        )}
                      </div>
                    )}
                  </div>
                  <button
                    type="button"
                    onClick={() => openBlindSpot(row.region.subSpace)}
                    data-ui-bridge-id="productivity.blind-spot-open"
                    className="inline-flex items-center gap-1 rounded-md border border-border bg-muted/20 px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/40 shrink-0"
                  >
                    Open
                    <ArrowRight className="w-3 h-3" />
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

export default BlindSpotsPanel;

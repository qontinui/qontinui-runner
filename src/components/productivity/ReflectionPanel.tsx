/**
 * ReflectionPanel — Phase 5 of the productivity-stack plan.
 *
 * Renders a per-plan reflection summary as an inline panel above the
 * task list on the plan/task board. Layout:
 *
 *   ┌────────────────────────────────────────────────────────────┐
 *   │ Header: plan title · status · short version-hash           │
 *   │ Top stat row: total / completed / escalated / avg conf /   │
 *   │               knowledge count                              │
 *   │ Top knowledge: top 5 entries (clickable → KnowledgeBrowser) │
 *   │ Review trail: chronological reviews list (clickable →      │
 *   │               worker session tab)                          │
 *   └────────────────────────────────────────────────────────────┘
 *
 * The panel is reusable: it only takes a `planId`. The parent provides
 * the plan row context (title, status, version hash) so the header
 * doesn't fan-out an extra DB call. Empty stats render gracefully
 * (avgConfidence = "—", knowledgeCount = 0).
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  RefreshCw,
  Sparkles,
  AlertTriangle,
  CheckCircle2,
  Loader2,
  BookOpen,
  ScrollText,
  ExternalLink,
} from "lucide-react";
import { getReflection, type Reflection } from "./reflectionApi";
import type { PlanRow } from "./types";
import type { ReviewVerdict } from "./reviewsApi";

interface ReflectionPanelProps {
  planId: string;
  /** Optional plan context for the header. When provided, the panel can
   *  render plan title + version hash without fetching the plan row. */
  plan?: PlanRow | null;
}

/** Verdict → tailwind classes for the small badge. */
const VERDICT_STYLES: Record<ReviewVerdict, string> = {
  approved: "bg-emerald-500/10 text-emerald-300 border-emerald-500/40",
  needs_fix: "bg-amber-500/10 text-amber-300 border-amber-500/40",
  escalate: "bg-red-500/10 text-red-300 border-red-500/40",
};

function formatConfidence(value: number | null): string {
  if (value === null || Number.isNaN(value)) return "—";
  return `${(value * 100).toFixed(0)}%`;
}

function shortHash(hash: string | undefined): string {
  if (!hash) return "";
  return hash.slice(0, 8);
}

interface StatTileProps {
  label: string;
  value: string;
  tone?: "default" | "good" | "warn" | "bad";
}

function StatTile({ label, value, tone = "default" }: StatTileProps) {
  const toneClass =
    tone === "good"
      ? "text-emerald-300"
      : tone === "warn"
        ? "text-amber-300"
        : tone === "bad"
          ? "text-red-300"
          : "text-foreground";
  return (
    <div className="flex flex-col items-start rounded-md border border-border/40 bg-background/40 px-3 py-2">
      <span className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</span>
      <span className={`text-base font-semibold ${toneClass}`}>{value}</span>
    </div>
  );
}

export function ReflectionPanel({ planId, plan }: ReflectionPanelProps) {
  const [reflection, setReflection] = useState<Reflection | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await getReflection(planId);
      setReflection(data);
    } catch (err) {
      setReflection(null);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [planId]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async data load on dependency change
    void load();
  }, [load]);

  const headerTitle = useMemo(() => {
    if (plan?.title) return plan.title;
    const fallback = plan?.markdownPath?.split(/[\\/]/).pop();
    return fallback ?? planId;
  }, [plan, planId]);

  const handleKnowledgeClick = useCallback((area: string) => {
    // Switch to the knowledge sub-view and pre-filter to the area. The
    // ProductivityPage / KnowledgeBrowser listen for these events.
    window.dispatchEvent(
      new CustomEvent("productivity-set-view", { detail: { view: "knowledge" } }),
    );
    window.dispatchEvent(
      new CustomEvent("productivity-knowledge-filter", {
        detail: { area },
      }),
    );
  }, []);

  const handleReviewClick = useCallback((sessionId: string) => {
    // Open the worker session in the Terminal tab. The existing terminal
    // page listens for this event (matches CoordinatorDashboard's
    // 'productivity-select-task' pattern).
    window.dispatchEvent(
      new CustomEvent("productivity-open-session", {
        detail: { sessionId },
      }),
    );
  }, []);

  return (
    <section
      className="flex flex-col rounded-lg border border-border bg-card/30 p-3 gap-3"
      data-ui-bridge-id="productivity.reflection-panel"
    >
      <header className="flex items-start justify-between gap-2">
        <div className="flex flex-col min-w-0">
          <div className="flex items-center gap-2">
            <Sparkles className="w-4 h-4 text-purple-400" />
            <h3 className="text-sm font-semibold text-foreground truncate">
              Reflection · {headerTitle}
            </h3>
            {plan?.status && (
              <span className="text-[10px] uppercase tracking-wider text-muted-foreground">
                {plan.status}
              </span>
            )}
            {plan?.versionHash && (
              <span
                className="font-mono text-[10px] text-muted-foreground"
                title={plan.versionHash}
              >
                {shortHash(plan.versionHash)}
              </span>
            )}
          </div>
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
          Reflection unavailable: {error}
        </div>
      ) : !reflection && loading ? (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 className="w-3 h-3 animate-spin" /> Loading reflection…
        </div>
      ) : !reflection ? (
        <div className="text-xs text-muted-foreground italic">
          No reflection data for this plan yet.
        </div>
      ) : (
        <>
          {/* Top stat row */}
          <div className="grid grid-cols-2 sm:grid-cols-5 gap-2">
            <StatTile label="Total tasks" value={String(reflection.totalTasks)} />
            <StatTile
              label="Completed"
              value={String(reflection.completedTasks)}
              tone={reflection.completedTasks > 0 ? "good" : "default"}
            />
            <StatTile
              label="Escalated"
              value={String(reflection.escalatedTasks)}
              tone={reflection.escalatedTasks > 0 ? "warn" : "default"}
            />
            <StatTile label="Avg confidence" value={formatConfidence(reflection.avgConfidence)} />
            <StatTile label="Knowledge" value={String(reflection.knowledgeCount)} />
          </div>

          {/* Top knowledge */}
          <section
            className="flex flex-col gap-1"
            data-ui-bridge-id="productivity.reflection-top-knowledge"
          >
            <div className="flex items-center gap-2">
              <BookOpen className="w-3.5 h-3.5 text-muted-foreground" />
              <h4 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Top knowledge
              </h4>
            </div>
            {reflection.topKnowledge.length === 0 ? (
              <p className="text-xs text-muted-foreground/70 italic">
                No knowledge entries linked to this plan's tasks yet.
              </p>
            ) : (
              <ul className="flex flex-col gap-1">
                {reflection.topKnowledge.map((row) => (
                  <li key={row.id}>
                    <button
                      type="button"
                      onClick={() => handleKnowledgeClick(row.area)}
                      className="w-full text-left rounded-md border border-border/30 bg-background/30 px-2 py-1.5 text-xs hover:bg-muted/30 transition-colors"
                    >
                      <span className="font-mono text-[10px] uppercase tracking-wider text-purple-300 mr-2">
                        {row.area}
                      </span>
                      <span className="text-foreground/90">{row.summary}</span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>

          {/* Review trail */}
          <section
            className="flex flex-col gap-1"
            data-ui-bridge-id="productivity.reflection-review-trail"
          >
            <div className="flex items-center gap-2">
              <ScrollText className="w-3.5 h-3.5 text-muted-foreground" />
              <h4 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Review trail
              </h4>
              <span className="text-[10px] text-muted-foreground">
                {reflection.reviewTrail.length} review
                {reflection.reviewTrail.length === 1 ? "" : "s"}
              </span>
            </div>
            {reflection.reviewTrail.length === 0 ? (
              <p className="text-xs text-muted-foreground/70 italic">
                No reviews yet. /auto-review writes verdicts here once tasks land in the `review`
                state.
              </p>
            ) : (
              <ul className="flex flex-col gap-1 overflow-auto pr-1" style={{ maxHeight: 200 }}>
                {reflection.reviewTrail.map((row) => {
                  const Icon =
                    row.verdict === "approved"
                      ? CheckCircle2
                      : row.verdict === "needs_fix"
                        ? AlertTriangle
                        : AlertTriangle;
                  return (
                    <li
                      key={row.reviewId}
                      className={`flex items-center gap-2 rounded-md border px-2 py-1 text-xs ${VERDICT_STYLES[row.verdict]}`}
                    >
                      <Icon className="w-3 h-3 shrink-0" />
                      <span className="font-mono text-[10px]">{row.verdict}</span>
                      <span className="text-foreground/80">
                        {(row.confidence * 100).toFixed(0)}%
                      </span>
                      <span className="text-muted-foreground text-[10px] truncate flex-1">
                        task {row.taskId.slice(0, 8)} · {new Date(row.createdAt).toLocaleString()}
                      </span>
                      <button
                        type="button"
                        onClick={() => handleReviewClick(row.taskId)}
                        className="inline-flex items-center gap-0.5 text-[10px] text-muted-foreground hover:text-foreground"
                        title="Open worker session"
                      >
                        <ExternalLink className="w-3 h-3" />
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>
        </>
      )}
    </section>
  );
}

export default ReflectionPanel;

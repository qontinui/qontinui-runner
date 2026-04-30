/**
 * CoordinatorDashboard — Phase 2 of the productivity-stack plan.
 *
 * Three sub-panels (vertical stack):
 *
 *   1. Recommendations queue — Phase-3 stub. Reviews land in Phase 3.
 *   2. Escalations — open destructive recommendations from
 *      `coordinator_decisions` (auto_acted=false AND action ∈ {escalate,
 *      kill-session, force-promote-to-worktree}).
 *   3. Decision log — chronological feed of last 200 rows, filterable by
 *      rule and action.
 *
 * Backend wiring: `coordinatorApi.ts`. The dashboard refreshes on mount
 * and on a manual refresh button; v1 doesn't subscribe to the
 * `coordinator-escalation` Tauri event yet (Phase 5 will).
 *
 * Per `proj_ui_bridge_sm_element_uniqueness.md`, every UI Bridge id below
 * lives only in the `productivity-coordinator` state in
 * `productivity.spec.uibridge.json`.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  RefreshCw,
  AlertTriangle,
  ListChecks,
  ScrollText,
  CheckCircle2,
  XCircle,
  ExternalLink,
} from "lucide-react";
import {
  getCoordinatorDecisions,
  getEscalations,
  resolveEscalation,
  type CoordinatorDecision,
  type Escalation,
} from "./coordinatorApi";
import {
  approveRecommendation,
  getRecommendations,
  rejectRecommendation,
  type Recommendation,
} from "./reviewsApi";
import { acknowledgeAdvisory } from "./reflectionApi";
import { PlanRecommendations } from "./PlanRecommendations";

const DECISION_LOG_LIMIT = 200;

/** Static action filter list — keeps the dropdown stable even if the
 *  feed is empty. Matches the `CoordinatorActionName` union. */
const ACTION_FILTER_OPTIONS = [
  "",
  "assign-task",
  "pause-session",
  "merge-task",
  "reassign-needs-fix",
  "escalate",
  "advise-with-text",
  "escalate-with-text",
  "kill-session",
  "force-promote-to-worktree",
  "cancel-task",
  "idle-no-action",
] as const;

const RULE_FILTER_OPTIONS = ["", "A", "B", "C", "D", "E", "LLM", "idle"] as const;

// ---------------------------------------------------------------------------
// Recommendations queue (Phase 3)
// ---------------------------------------------------------------------------

interface RecommendationsPanelProps {
  rows: Recommendation[];
  loading: boolean;
  error: string | null;
  highlightedReviewId: string | null;
  onRefresh: () => void;
  onApprove: (reviewId: string) => Promise<void>;
  onReject: (reviewId: string) => Promise<void>;
}

function RecommendationsPanel({
  rows,
  loading,
  error,
  highlightedReviewId,
  onRefresh,
  onApprove,
  onReject,
}: RecommendationsPanelProps) {
  const [busyId, setBusyId] = useState<string | null>(null);

  const handleApprove = async (reviewId: string) => {
    setBusyId(reviewId);
    try {
      await onApprove(reviewId);
    } finally {
      setBusyId(null);
    }
  };

  const handleReject = async (reviewId: string) => {
    setBusyId(reviewId);
    try {
      await onReject(reviewId);
    } finally {
      setBusyId(null);
    }
  };

  return (
    <section
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.coord-recommendations"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <ListChecks className="w-4 h-4 text-muted-foreground" />
          <h2 className="text-sm font-semibold text-foreground">Recommendations queue</h2>
          <span className="text-xs text-muted-foreground">{rows.length} pending</span>
        </div>
        <button
          type="button"
          onClick={onRefresh}
          disabled={loading}
          data-ui-bridge-id="productivity.coord-recommendations-refresh"
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
      ) : rows.length === 0 ? (
        <div className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground">
          No medium-confidence approvals waiting. /auto-review verdicts in the [0.7, 0.85)
          confidence band queue here for your thumbs-up before auto-merge.
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {rows.map((row) => {
            const isHighlighted = row.id === highlightedReviewId;
            const reasoningSnippet = row.reasoning
              .split(/\r?\n/)
              .find((l) => l.trim().length > 0)
              ?.slice(0, 240);
            return (
              <li
                key={row.id}
                className={`rounded-md border p-3 transition-colors ${
                  isHighlighted
                    ? "border-amber-400 bg-amber-500/10"
                    : "border-border/40 bg-background/40"
                }`}
                data-ui-bridge-id="productivity.coord-recommendation-card"
                data-review-id={row.id}
              >
                <div className="flex items-start justify-between gap-3">
                  <div className="flex flex-col gap-1 min-w-0">
                    <div className="flex items-center gap-2 text-xs">
                      <span className="font-mono text-amber-300">{row.verdict}</span>
                      <span className="text-muted-foreground">
                        confidence {(row.confidence * 100).toFixed(0)}%
                      </span>
                      <span className="text-muted-foreground">
                        {new Date(row.createdAt).toLocaleString()}
                      </span>
                    </div>
                    <div className="text-xs text-muted-foreground truncate">
                      session <span className="font-mono">{row.reviewedSessionId}</span>
                    </div>
                    {row.planTitle && (
                      <div className="text-xs text-muted-foreground truncate">
                        plan: {row.planTitle}
                      </div>
                    )}
                    <div className="text-xs text-foreground/90 line-clamp-2">
                      {row.taskDescription}
                    </div>
                    {reasoningSnippet && (
                      <p className="text-xs text-foreground/70 italic">{reasoningSnippet}</p>
                    )}
                  </div>
                  <div className="flex flex-col gap-1 shrink-0">
                    <button
                      type="button"
                      disabled={busyId === row.id}
                      onClick={() => handleApprove(row.id)}
                      data-ui-bridge-id="productivity.coord-recommendation-approve"
                      className="inline-flex items-center gap-1 rounded-md border border-emerald-500/40 bg-emerald-500/10 px-2 py-1 text-xs text-emerald-300 hover:bg-emerald-500/20 disabled:opacity-50"
                    >
                      <CheckCircle2 className="w-3 h-3" /> Approve
                    </button>
                    <button
                      type="button"
                      disabled={busyId === row.id}
                      onClick={() => handleReject(row.id)}
                      data-ui-bridge-id="productivity.coord-recommendation-reject"
                      className="inline-flex items-center gap-1 rounded-md border border-red-500/40 bg-red-500/10 px-2 py-1 text-xs text-red-300 hover:bg-red-500/20 disabled:opacity-50"
                    >
                      <XCircle className="w-3 h-3" /> Reject
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        // Switch back to the plans sub-view with this task
                        // pre-selected. The Productivity tab listens for
                        // both events.
                        window.dispatchEvent(
                          new CustomEvent("productivity-set-view", {
                            detail: { view: "plans" },
                          }),
                        );
                        window.dispatchEvent(
                          new CustomEvent("productivity-select-task", {
                            detail: { taskId: row.taskId },
                          }),
                        );
                      }}
                      data-ui-bridge-id="productivity.coord-recommendation-open-task"
                      className="inline-flex items-center gap-1 rounded-md border border-border bg-muted/20 px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/40"
                    >
                      <ExternalLink className="w-3 h-3" /> Open task
                    </button>
                  </div>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Escalations panel
// ---------------------------------------------------------------------------

interface EscalationsPanelProps {
  rows: Escalation[];
  loading: boolean;
  error: string | null;
  onRefresh: () => void;
  onResolve: (id: string, resolution: string) => Promise<void>;
}

function EscalationsPanel({ rows, loading, error, onRefresh, onResolve }: EscalationsPanelProps) {
  const [busyId, setBusyId] = useState<string | null>(null);

  const handleResolve = async (row: Escalation, decision: "approve" | "decline") => {
    setBusyId(row.id);
    try {
      await onResolve(row.id, decision);
    } finally {
      setBusyId(null);
    }
  };

  return (
    <section
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.coord-escalations"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <AlertTriangle className="w-4 h-4 text-amber-400" />
          <h2 className="text-sm font-semibold text-foreground">Escalations</h2>
          <span className="text-xs text-muted-foreground">{rows.length} open</span>
        </div>
        <button
          type="button"
          onClick={onRefresh}
          disabled={loading}
          data-ui-bridge-id="productivity.coord-escalations-refresh"
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
      ) : rows.length === 0 ? (
        <div className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground">
          No open escalations. The Coordinator only routes destructive actions (kill-session,
          force-promote-to-worktree) here; everything else auto-acts.
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {rows.map((row) => (
            <li
              key={row.id}
              className="rounded-md border border-amber-500/30 bg-amber-500/5 p-3"
              data-ui-bridge-id="productivity.coord-escalation-card"
            >
              <div className="flex items-start justify-between gap-3">
                <div className="flex flex-col gap-1 min-w-0">
                  <div className="flex items-center gap-2 text-xs">
                    <span className="font-mono text-amber-400">{row.action}</span>
                    <span className="text-muted-foreground">·</span>
                    <span className="text-muted-foreground">rule {row.rule}</span>
                    <span className="text-muted-foreground">·</span>
                    <span className="text-muted-foreground">
                      {new Date(row.createdAt).toLocaleString()}
                    </span>
                  </div>
                  {row.targetId && (
                    <div className="text-xs text-muted-foreground">
                      target: <span className="font-mono">{row.targetId}</span>
                    </div>
                  )}
                  <p className="text-sm text-foreground whitespace-pre-wrap break-words">
                    {row.reasoning}
                  </p>
                </div>
                <div className="flex flex-col gap-1 shrink-0">
                  <button
                    type="button"
                    disabled={busyId === row.id}
                    onClick={() => handleResolve(row, "approve")}
                    className="inline-flex items-center gap-1 rounded-md border border-emerald-500/40 bg-emerald-500/10 px-2 py-1 text-xs text-emerald-300 hover:bg-emerald-500/20 disabled:opacity-50"
                  >
                    <CheckCircle2 className="w-3 h-3" /> Approve
                  </button>
                  <button
                    type="button"
                    disabled={busyId === row.id}
                    onClick={() => handleResolve(row, "decline")}
                    className="inline-flex items-center gap-1 rounded-md border border-red-500/40 bg-red-500/10 px-2 py-1 text-xs text-red-300 hover:bg-red-500/20 disabled:opacity-50"
                  >
                    <XCircle className="w-3 h-3" /> Decline
                  </button>
                </div>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Advisories panel — Phase 5
//
// Surfaces LLM-driven `advise-with-text` decisions from the last hour.
// Each card has reasoning + an Acknowledge button. The same
// `coordinator_decisions.resolved` flag is flipped on acknowledgement
// (via `acknowledgeAdvisory`), so once a row is acknowledged it stops
// appearing here.
// ---------------------------------------------------------------------------

interface AdvisoriesPanelProps {
  rows: CoordinatorDecision[];
  loading: boolean;
  error: string | null;
  onRefresh: () => void;
  onAcknowledge: (decisionId: string) => Promise<void>;
}

function AdvisoriesPanel({ rows, loading, error, onRefresh, onAcknowledge }: AdvisoriesPanelProps) {
  const [busyId, setBusyId] = useState<string | null>(null);

  const handleAcknowledge = async (id: string) => {
    setBusyId(id);
    try {
      await onAcknowledge(id);
    } finally {
      setBusyId(null);
    }
  };

  return (
    <section
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.coord-advisories"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <ListChecks className="w-4 h-4 text-purple-400" />
          <h2 className="text-sm font-semibold text-foreground">Advisories</h2>
          <span className="text-xs text-muted-foreground">{rows.length} unread</span>
        </div>
        <button
          type="button"
          onClick={onRefresh}
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
      ) : rows.length === 0 ? (
        <div className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground">
          No advisories. The Coordinator's LLM branch writes here when it can't pick
          deterministically — `advise-with-text` rows from the last hour land here for your
          awareness.
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {rows.map((row) => (
            <li
              key={row.id}
              className="rounded-md border border-purple-500/30 bg-purple-500/5 p-3"
              data-ui-bridge-id="productivity.coord-advisory-card"
            >
              <div className="flex items-start justify-between gap-3">
                <div className="flex flex-col gap-1 min-w-0">
                  <div className="flex items-center gap-2 text-xs">
                    <span className="font-mono text-purple-300">{row.action}</span>
                    <span className="text-muted-foreground">·</span>
                    <span className="text-muted-foreground">rule {row.rule}</span>
                    <span className="text-muted-foreground">·</span>
                    <span className="text-muted-foreground">
                      {new Date(row.createdAt).toLocaleString()}
                    </span>
                  </div>
                  {row.targetId && (
                    <div className="text-xs text-muted-foreground">
                      target: <span className="font-mono">{row.targetId}</span>
                    </div>
                  )}
                  <p className="text-sm text-foreground whitespace-pre-wrap break-words">
                    {row.reasoning}
                  </p>
                </div>
                <button
                  type="button"
                  disabled={busyId === row.id}
                  onClick={() => handleAcknowledge(row.id)}
                  className="inline-flex items-center gap-1 rounded-md border border-emerald-500/40 bg-emerald-500/10 px-2 py-1 text-xs text-emerald-300 hover:bg-emerald-500/20 disabled:opacity-50 shrink-0"
                >
                  <CheckCircle2 className="w-3 h-3" /> Acknowledge
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Decision log
// ---------------------------------------------------------------------------

interface DecisionLogPanelProps {
  rows: CoordinatorDecision[];
  loading: boolean;
  error: string | null;
  ruleFilter: string;
  actionFilter: string;
  onRuleFilterChange: (v: string) => void;
  onActionFilterChange: (v: string) => void;
  onRefresh: () => void;
}

function DecisionLogPanel({
  rows,
  loading,
  error,
  ruleFilter,
  actionFilter,
  onRuleFilterChange,
  onActionFilterChange,
  onRefresh,
}: DecisionLogPanelProps) {
  return (
    <section
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3 min-h-0"
      data-ui-bridge-id="productivity.coord-decision-log"
    >
      <header className="flex items-center justify-between flex-wrap gap-2">
        <div className="flex items-center gap-2">
          <ScrollText className="w-4 h-4 text-muted-foreground" />
          <h2 className="text-sm font-semibold text-foreground">Decision log</h2>
          <span className="text-xs text-muted-foreground">last {DECISION_LOG_LIMIT} rows</span>
        </div>
        <div className="flex items-center gap-2">
          <label className="text-xs text-muted-foreground">
            rule
            <select
              value={ruleFilter}
              onChange={(e) => onRuleFilterChange(e.target.value)}
              data-ui-bridge-id="productivity.coord-decision-log-rule-filter"
              className="ml-1 rounded-md border border-border bg-background px-2 py-1 text-xs text-foreground"
            >
              {RULE_FILTER_OPTIONS.map((opt) => (
                <option key={opt} value={opt}>
                  {opt === "" ? "all" : opt}
                </option>
              ))}
            </select>
          </label>
          <label className="text-xs text-muted-foreground">
            action
            <select
              value={actionFilter}
              onChange={(e) => onActionFilterChange(e.target.value)}
              data-ui-bridge-id="productivity.coord-decision-log-action-filter"
              className="ml-1 rounded-md border border-border bg-background px-2 py-1 text-xs text-foreground"
            >
              {ACTION_FILTER_OPTIONS.map((opt) => (
                <option key={opt} value={opt}>
                  {opt === "" ? "all" : opt}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            onClick={onRefresh}
            disabled={loading}
            data-ui-bridge-id="productivity.coord-decision-log-refresh"
            className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 disabled:opacity-50"
          >
            <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
            Refresh
          </button>
        </div>
      </header>

      {error ? (
        <div className="rounded-md border border-red-500/30 bg-red-500/10 p-3 text-xs text-red-400">
          {error}
        </div>
      ) : rows.length === 0 ? (
        <div className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground">
          No coordinator decisions yet. Once `/coordinate` is running, every rule fire (including
          no-ops) writes a row here for audit.
        </div>
      ) : (
        <ul className="flex flex-col gap-1 overflow-auto pr-1" style={{ maxHeight: "60vh" }}>
          {rows.map((row) => (
            <li
              key={row.id}
              className="flex items-start justify-between gap-3 rounded-md border border-border/40 bg-background/40 px-3 py-2"
            >
              <div className="flex flex-col gap-0.5 min-w-0">
                <div className="flex items-center gap-2 text-xs">
                  <span
                    className={`font-mono ${row.autoActed ? "text-emerald-400" : "text-amber-400"}`}
                  >
                    {row.action}
                  </span>
                  <span className="text-muted-foreground">rule {row.rule}</span>
                  <span className="text-muted-foreground">iter #{row.iteration}</span>
                  <span className="text-muted-foreground">
                    {new Date(row.createdAt).toLocaleTimeString()}
                  </span>
                </div>
                {row.targetId && (
                  <div className="text-[11px] text-muted-foreground font-mono truncate">
                    {row.targetId}
                  </div>
                )}
                <p className="text-xs text-foreground/80 whitespace-pre-wrap break-words">
                  {row.reasoning}
                </p>
              </div>
              <span
                className={`shrink-0 rounded-full border px-2 py-0.5 text-[10px] font-medium ${
                  row.autoActed
                    ? "border-emerald-500/40 bg-emerald-500/10 text-emerald-300"
                    : "border-amber-500/40 bg-amber-500/10 text-amber-300"
                }`}
              >
                {row.autoActed ? "auto" : "advisory"}
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Top-level dashboard
// ---------------------------------------------------------------------------

export function CoordinatorDashboard() {
  const [decisions, setDecisions] = useState<CoordinatorDecision[]>([]);
  const [escalations, setEscalations] = useState<Escalation[]>([]);
  const [recommendations, setRecommendations] = useState<Recommendation[]>([]);
  const [advisories, setAdvisories] = useState<CoordinatorDecision[]>([]);
  const [decisionsLoading, setDecisionsLoading] = useState(false);
  const [escalationsLoading, setEscalationsLoading] = useState(false);
  const [recommendationsLoading, setRecommendationsLoading] = useState(false);
  const [advisoriesLoading, setAdvisoriesLoading] = useState(false);
  const [decisionsError, setDecisionsError] = useState<string | null>(null);
  const [escalationsError, setEscalationsError] = useState<string | null>(null);
  const [recommendationsError, setRecommendationsError] = useState<string | null>(null);
  const [advisoriesError, setAdvisoriesError] = useState<string | null>(null);
  const [ruleFilter, setRuleFilter] = useState("");
  const [actionFilter, setActionFilter] = useState("");
  const [highlightedReviewId, setHighlightedReviewId] = useState<string | null>(null);

  const loadDecisions = useCallback(async () => {
    setDecisionsLoading(true);
    setDecisionsError(null);
    try {
      const rows = await getCoordinatorDecisions(DECISION_LOG_LIMIT, {
        rule: ruleFilter,
        action: actionFilter,
      });
      setDecisions(rows);
    } catch (err) {
      // Backend may not yet have the v29 migration applied (e.g. older
      // runner build). Render the soft empty state rather than crashing.
      setDecisionsError(err instanceof Error ? err.message : "Failed to load decisions");
      setDecisions([]);
    } finally {
      setDecisionsLoading(false);
    }
  }, [ruleFilter, actionFilter]);

  const loadEscalations = useCallback(async () => {
    setEscalationsLoading(true);
    setEscalationsError(null);
    try {
      const rows = await getEscalations();
      setEscalations(rows);
    } catch (err) {
      setEscalationsError(err instanceof Error ? err.message : "Failed to load escalations");
      setEscalations([]);
    } finally {
      setEscalationsLoading(false);
    }
  }, []);

  const loadRecommendations = useCallback(async () => {
    setRecommendationsLoading(true);
    setRecommendationsError(null);
    try {
      const rows = await getRecommendations();
      setRecommendations(rows);
    } catch (err) {
      // v31 may not yet be applied (older runner build) — render the empty
      // state rather than crashing. Same shape as decisions/escalations.
      setRecommendationsError(
        err instanceof Error ? err.message : "Failed to load recommendations",
      );
      setRecommendations([]);
    } finally {
      setRecommendationsLoading(false);
    }
  }, []);

  // Load advisories: `advise-with-text` rows from the last hour that
  // haven't been acknowledged. Filter client-side from the
  // action-filtered decision feed so we don't add a sibling endpoint.
  const loadAdvisories = useCallback(async () => {
    setAdvisoriesLoading(true);
    setAdvisoriesError(null);
    try {
      const rows = await getCoordinatorDecisions(50, {
        action: "advise-with-text",
      });
      const cutoff = Date.now() - 60 * 60 * 1000;
      const filtered = rows.filter((r) => !r.resolved && new Date(r.createdAt).getTime() >= cutoff);
      setAdvisories(filtered);
    } catch (err) {
      setAdvisoriesError(err instanceof Error ? err.message : "Failed to load advisories");
      setAdvisories([]);
    } finally {
      setAdvisoriesLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async data load on dependency change
    void loadDecisions();
  }, [loadDecisions]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async data load on dependency change
    void loadEscalations();
  }, [loadEscalations]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async data load on dependency change
    void loadRecommendations();
  }, [loadRecommendations]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async data load on dependency change
    void loadAdvisories();
  }, [loadAdvisories]);

  // Subscribe to `review-completed` so a finished /auto-review surfaces
  // here without forcing the user to refresh. Tauri may not be present in
  // tests; tolerate that by no-op'ing the listener.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    listen("review-completed", () => {
      if (!cancelled) void loadRecommendations();
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [loadRecommendations]);

  // Subscribe to `coordinator-advice` so the LLM-driven advise-with-text
  // rows appear without polling. Same tolerant pattern as
  // review-completed.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;
    listen("coordinator-advice", () => {
      if (!cancelled) void loadAdvisories();
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [loadAdvisories]);

  // The ReviewBadge dispatches `productivity-select-recommendation` with a
  // review id when the user clicks a medium-confidence badge. Highlight
  // that row in the queue on arrival.
  useEffect(() => {
    function onSelect(e: Event) {
      const detail = (e as CustomEvent<{ reviewId?: string }>).detail;
      if (detail?.reviewId) {
        setHighlightedReviewId(detail.reviewId);
        // Scroll the queue's matching card into view if present.
        requestAnimationFrame(() => {
          const card = document.querySelector(
            `[data-ui-bridge-id="productivity.coord-recommendation-card"][data-review-id="${detail.reviewId}"]`,
          );
          card?.scrollIntoView({ behavior: "smooth", block: "center" });
        });
      }
    }
    window.addEventListener("productivity-select-recommendation", onSelect);
    return () => window.removeEventListener("productivity-select-recommendation", onSelect);
  }, []);

  const handleResolve = useCallback(
    async (id: string, resolution: string) => {
      try {
        await resolveEscalation(id, resolution);
      } catch (err) {
        setEscalationsError(err instanceof Error ? err.message : "Failed to resolve escalation");
        return;
      }
      await loadEscalations();
      await loadDecisions();
    },
    [loadDecisions, loadEscalations],
  );

  const handleApproveRecommendation = useCallback(
    async (reviewId: string) => {
      try {
        await approveRecommendation(reviewId);
      } catch (err) {
        setRecommendationsError(
          err instanceof Error ? err.message : "Failed to approve recommendation",
        );
        return;
      }
      await loadRecommendations();
      // Approve flips the task to done; refresh the decision log too so
      // the audit trail reflects it.
      await loadDecisions();
    },
    [loadRecommendations, loadDecisions],
  );

  const handleRejectRecommendation = useCallback(
    async (reviewId: string) => {
      try {
        await rejectRecommendation(reviewId);
      } catch (err) {
        setRecommendationsError(
          err instanceof Error ? err.message : "Failed to reject recommendation",
        );
        return;
      }
      await loadRecommendations();
    },
    [loadRecommendations],
  );

  const handleAcknowledgeAdvisory = useCallback(
    async (decisionId: string) => {
      try {
        await acknowledgeAdvisory(decisionId);
      } catch (err) {
        setAdvisoriesError(err instanceof Error ? err.message : "Failed to acknowledge advisory");
        return;
      }
      await loadAdvisories();
      // Acknowledgement flips `resolved`; refresh the audit log too.
      await loadDecisions();
    },
    [loadAdvisories, loadDecisions],
  );

  // Memoise the list so the table doesn't re-render when other panels
  // update.
  const decisionRows = useMemo(() => decisions, [decisions]);
  const escalationRows = useMemo(() => escalations, [escalations]);
  const recommendationRows = useMemo(() => recommendations, [recommendations]);
  const advisoryRows = useMemo(() => advisories, [advisories]);

  return (
    <div className="h-full overflow-auto bg-background" data-page-id="productivity-coordinator">
      <div className="flex flex-col gap-4 p-4 max-w-5xl mx-auto">
        <header>
          <h1 className="text-lg font-semibold text-foreground">Coordinator dashboard</h1>
          <p className="text-sm text-muted-foreground">
            Cheap-rule decisions auto-act; destructive recommendations (kill-session,
            force-promote-to-worktree) escalate here for your approval.
          </p>
        </header>

        <PlanRecommendations />

        <RecommendationsPanel
          rows={recommendationRows}
          loading={recommendationsLoading}
          error={recommendationsError}
          highlightedReviewId={highlightedReviewId}
          onRefresh={loadRecommendations}
          onApprove={handleApproveRecommendation}
          onReject={handleRejectRecommendation}
        />

        <AdvisoriesPanel
          rows={advisoryRows}
          loading={advisoriesLoading}
          error={advisoriesError}
          onRefresh={loadAdvisories}
          onAcknowledge={handleAcknowledgeAdvisory}
        />

        <EscalationsPanel
          rows={escalationRows}
          loading={escalationsLoading}
          error={escalationsError}
          onRefresh={loadEscalations}
          onResolve={handleResolve}
        />

        <DecisionLogPanel
          rows={decisionRows}
          loading={decisionsLoading}
          error={decisionsError}
          ruleFilter={ruleFilter}
          actionFilter={actionFilter}
          onRuleFilterChange={setRuleFilter}
          onActionFilterChange={setActionFilter}
          onRefresh={loadDecisions}
        />
      </div>
    </div>
  );
}

export default CoordinatorDashboard;

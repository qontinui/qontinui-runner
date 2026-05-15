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
import { formatDistanceToNowStrict } from "date-fns";
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
  dispatchCoordinatorAction,
  getCoordinatorDecisions,
  getCoordinatorLeader,
  getCoordinatorState,
  getEscalations,
  launchCoordinatorSession,
  resolveEscalation,
  spawnWorkerSession,
  stopCoordinatorSession,
  type CoordinatorDecision,
  type CoordinatorLaunchMode,
  type CoordinatorLeaderRow,
  type Escalation,
  type LeaderResponse,
  type LiveSession,
} from "./coordinatorApi";
import {
  approveRecommendation,
  getRecommendations,
  rejectRecommendation,
  type Recommendation,
} from "./reviewsApi";
import { acknowledgeAdvisory } from "./reflectionApi";
import { PlanRecommendations } from "./PlanRecommendations";
import { BlindSpotsPanel } from "./BlindSpotsPanel";
import { WorkersPanel } from "./WorkersPanel";
import { FileActivityPanel } from "./FileActivityPanel";
import { FleetHealthPanel } from "./FleetHealthPanel";

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

/** Source filter — partitions the decision log by writer. The deconflicter
 *  is a Rust-side loop that writes rows with `session_id` prefixed by
 *  `"deconflicter-"` (see §3.1 of the deconflicter plan); the AI Coordinator
 *  PTY skill writes rows with a Claude session UUID. Filter is client-side
 *  only — no new endpoint. */
type SourceFilterKey = "all" | "ai" | "deconflicter";

const SOURCE_FILTER_OPTIONS: ReadonlyArray<{ key: SourceFilterKey; label: string }> = [
  { key: "all", label: "All" },
  { key: "ai", label: "AI Coordinator" },
  { key: "deconflicter", label: "Deconflicter" },
] as const;

const DECONFLICTER_SESSION_PREFIX = "deconflicter-";

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
      role="region"
      aria-labelledby="productivity-coord-recommendations-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.coord-recommendations"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <ListChecks className="w-4 h-4 text-muted-foreground" />
          <h2
            id="productivity-coord-recommendations-heading"
            className="text-sm font-semibold text-foreground"
          >
            Recommendations queue
          </h2>
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
      role="region"
      aria-labelledby="productivity-coord-escalations-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.coord-escalations"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <AlertTriangle className="w-4 h-4 text-amber-400" />
          <h2
            id="productivity-coord-escalations-heading"
            className="text-sm font-semibold text-foreground"
          >
            Escalations
          </h2>
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
      role="region"
      aria-labelledby="productivity-coord-advisories-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.coord-advisories"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <ListChecks className="w-4 h-4 text-purple-400" />
          <h2
            id="productivity-coord-advisories-heading"
            className="text-sm font-semibold text-foreground"
          >
            Advisories
          </h2>
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
// Deferrals panel — coordinator-driven-scheduling-plan Phase 3
//
// Lists Rule B advisories whose reasoning matches `/deferred/` — these are
// tasks the scheduler chose NOT to assign this tick because either
// (a) every candidate session collides with a held lock, or (b) an earlier
// task in the same iteration already claims overlapping paths. Grouped by
// `targetId` so a task gets ONE row regardless of how many deferral
// advisories its lifecycle produces.
//
// Each row's "Force-assign anyway" button opens a session-picker (idle
// `LiveSession`s from `/coordinator/state`) and fires
// `/coordinator/dispatch-action` with `AssignTask`. The user override is
// audited as `rule = "manual"` so the Decision Log shows it as a manual
// fire alongside the original deferral advisory.
// ---------------------------------------------------------------------------

/** Parse a deferral advisory's `reasoning` body to extract a
 *  human-readable blocker label. Two templates are emitted by Rule B in
 *  decide.rs (Phase 1 + Phase 2 of the plan):
 *
 *    "task {id} deferred — every candidate session would collide with
 *     held lock on {paths}"             → "lock on {paths}"
 *    "task {id} deferred behind task {Y} — overlaps {paths}"
 *                                       → "task {Y}"
 *
 *  Returns `null` when neither template matches — the row is skipped.
 *  The `[rule=B v=YYYY-MM-DD]` stamp prefix is optional, since the
 *  RULE_VERSION bumps over time.
 */
function extractDeferralBlocker(reasoning: string): string | null {
  const lockMatch = reasoning.match(
    /deferred — every candidate session would collide with held lock on (.+)$/,
  );
  if (lockMatch) return `lock on ${lockMatch[1].trim()}`;
  const taskMatch = reasoning.match(/deferred behind task ([^\s—]+)/);
  if (taskMatch) return `task ${taskMatch[1]}`;
  return null;
}

interface DeferralRow {
  taskId: string;
  blockerLabel: string;
  latestAt: string;
  reasoning: string;
  count: number;
}

/** Group the decision feed into one row per deferred task. Picks the
 *  newest deferral per `targetId` so the blocker label and timestamp
 *  reflect the most-recent advisory. */
function groupDeferrals(decisions: CoordinatorDecision[]): DeferralRow[] {
  const groups = new Map<string, DeferralRow>();
  for (const d of decisions) {
    if (d.rule !== "B" || d.action !== "advise-with-text" || !d.targetId) continue;
    const blockerLabel = extractDeferralBlocker(d.reasoning);
    if (!blockerLabel) continue;
    const existing = groups.get(d.targetId);
    if (!existing) {
      groups.set(d.targetId, {
        taskId: d.targetId,
        blockerLabel,
        latestAt: d.createdAt,
        reasoning: d.reasoning,
        count: 1,
      });
    } else {
      existing.count += 1;
      if (new Date(d.createdAt).getTime() > new Date(existing.latestAt).getTime()) {
        existing.latestAt = d.createdAt;
        existing.blockerLabel = blockerLabel;
        existing.reasoning = d.reasoning;
      }
    }
  }
  return Array.from(groups.values()).sort(
    (a, b) => new Date(b.latestAt).getTime() - new Date(a.latestAt).getTime(),
  );
}

interface DeferralsPanelProps {
  rows: DeferralRow[];
  loading: boolean;
  error: string | null;
  onRefresh: () => void;
  onForceAssign: (taskId: string, sessionId: string) => Promise<void>;
}

function DeferralsPanel({ rows, loading, error, onRefresh, onForceAssign }: DeferralsPanelProps) {
  const [pickerTaskId, setPickerTaskId] = useState<string | null>(null);
  const [sessions, setSessions] = useState<LiveSession[]>([]);
  const [pickerError, setPickerError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const openPicker = useCallback(async (taskId: string) => {
    setPickerError(null);
    setPickerTaskId(taskId);
    setSessions([]);
    try {
      const state = await getCoordinatorState();
      // Idle sessions only — Ready and not assigned. Same filter the
      // backend Rule B's `is_idle` predicate uses, but client-side here
      // since `/coordinator/state` doesn't expose `assignedTaskId` per
      // session today. We filter by `state === "Ready"` and let the
      // user pick — dispatch will fail server-side if the session has
      // since transitioned.
      setSessions(state.liveSessions.filter((s) => s.state === "Ready"));
    } catch (err) {
      setPickerError(err instanceof Error ? err.message : "Failed to load sessions");
    }
  }, []);

  const closePicker = () => {
    setPickerTaskId(null);
    setSessions([]);
    setPickerError(null);
  };

  const handlePick = async (sessionId: string) => {
    if (!pickerTaskId) return;
    setBusy(true);
    try {
      await onForceAssign(pickerTaskId, sessionId);
      closePicker();
    } catch (err) {
      setPickerError(err instanceof Error ? err.message : "Force-assign failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section
      role="region"
      aria-labelledby="productivity-coord-deferrals-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.coord-deferrals"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <AlertTriangle className="w-4 h-4 text-amber-400" />
          <h2
            id="productivity-coord-deferrals-heading"
            className="text-sm font-semibold text-foreground"
          >
            Deferred tasks
          </h2>
          <span className="text-xs text-muted-foreground">{rows.length} waiting</span>
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
          No deferred tasks. Rule B routes new work around held locks and overlapping ready tasks
          automatically — entries here are tasks the scheduler chose to wait on rather than race.
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {rows.map((row) => (
            <li
              key={row.taskId}
              className="rounded-md border border-amber-500/30 bg-amber-500/5 p-3"
              data-ui-bridge-id="productivity.coord-deferral-card"
              data-task-id={row.taskId}
            >
              <div className="flex items-start justify-between gap-3">
                <div className="flex flex-col gap-1 min-w-0">
                  <div className="flex items-center gap-2 text-xs">
                    <span className="font-mono text-amber-300">{row.taskId}</span>
                    <span className="text-muted-foreground">·</span>
                    <span className="text-muted-foreground">
                      deferred until {row.blockerLabel}
                    </span>
                  </div>
                  <div className="text-xs text-muted-foreground">
                    last advised{" "}
                    {formatDistanceToNowStrict(new Date(row.latestAt), { addSuffix: true })}
                    {row.count > 1 ? ` · ${row.count} advisories` : ""}
                  </div>
                  <p className="text-xs text-muted-foreground whitespace-pre-wrap break-words font-mono">
                    {row.reasoning}
                  </p>
                </div>
                <button
                  type="button"
                  disabled={busy && pickerTaskId === row.taskId}
                  onClick={() => openPicker(row.taskId)}
                  className="inline-flex items-center gap-1 rounded-md border border-amber-500/40 bg-amber-500/10 px-2 py-1 text-xs text-amber-300 hover:bg-amber-500/20 disabled:opacity-50 shrink-0"
                  data-ui-bridge-id="productivity.coord-deferral-force-assign"
                >
                  Force-assign anyway
                </button>
              </div>
              {pickerTaskId === row.taskId ? (
                <div
                  className="mt-3 rounded-md border border-border bg-card p-3 flex flex-col gap-2"
                  data-ui-bridge-id="productivity.coord-deferral-picker"
                >
                  <div className="flex items-center justify-between">
                    <span className="text-xs font-medium text-foreground">
                      Pick a session to assign {row.taskId}
                    </span>
                    <button
                      type="button"
                      onClick={closePicker}
                      className="text-xs text-muted-foreground hover:text-foreground"
                    >
                      Cancel
                    </button>
                  </div>
                  {pickerError ? (
                    <div className="text-xs text-red-400">{pickerError}</div>
                  ) : sessions.length === 0 ? (
                    <div className="text-xs text-muted-foreground">
                      No idle Ready sessions available. Spawn a worker tab first.
                    </div>
                  ) : (
                    <ul className="flex flex-col gap-1">
                      {sessions.map((s) => (
                        <li key={s.taskRunId}>
                          <button
                            type="button"
                            disabled={busy}
                            onClick={() => handlePick(s.taskRunId)}
                            className="w-full text-left rounded-md border border-border/60 bg-muted/20 px-2 py-1 text-xs font-mono text-foreground hover:bg-muted/40 disabled:opacity-50"
                          >
                            {s.taskRunId}
                            {s.title ? (
                              <span className="ml-2 text-muted-foreground">— {s.title}</span>
                            ) : null}
                          </button>
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              ) : null}
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
  sourceFilter: SourceFilterKey;
  onRuleFilterChange: (v: string) => void;
  onActionFilterChange: (v: string) => void;
  onSourceFilterChange: (v: SourceFilterKey) => void;
  onRefresh: () => void;
}

function DecisionLogPanel({
  rows,
  loading,
  error,
  ruleFilter,
  actionFilter,
  sourceFilter,
  onRuleFilterChange,
  onActionFilterChange,
  onSourceFilterChange,
  onRefresh,
}: DecisionLogPanelProps) {
  return (
    <section
      role="region"
      aria-labelledby="productivity-coord-decision-log-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3 min-h-0"
      data-ui-bridge-id="productivity.coord-decision-log"
    >
      <header className="flex items-center justify-between flex-wrap gap-2">
        <div className="flex items-center gap-2">
          <ScrollText className="w-4 h-4 text-muted-foreground" />
          <h2
            id="productivity-coord-decision-log-heading"
            className="text-sm font-semibold text-foreground"
          >
            Decision log
          </h2>
          <span className="text-xs text-muted-foreground">last {DECISION_LOG_LIMIT} rows</span>
        </div>
        <div className="flex items-center gap-2 flex-wrap">
          <div
            className="inline-flex items-center gap-1 rounded-md border border-border bg-background p-0.5"
            role="group"
            aria-label="source filter"
            data-ui-bridge-id="productivity.decision-source-filter"
          >
            {SOURCE_FILTER_OPTIONS.map((opt) => {
              const active = sourceFilter === opt.key;
              return (
                <button
                  key={opt.key}
                  type="button"
                  onClick={() => onSourceFilterChange(opt.key)}
                  aria-pressed={active}
                  data-source-filter-key={opt.key}
                  className={`rounded px-2 py-0.5 text-xs ${
                    active
                      ? "bg-muted/50 text-foreground"
                      : "text-muted-foreground hover:text-foreground hover:bg-muted/30"
                  }`}
                >
                  {opt.label}
                </button>
              );
            })}
          </div>
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
// Lease status pill (Phase 3 — launch controls)
// ---------------------------------------------------------------------------

interface CoordinatorLeaseStatusPillProps {
  status: "active" | "stale" | "vacant";
  leader: CoordinatorLeaderRow | null;
}

function safeFormatAge(iso: string | undefined | null): string {
  if (!iso) return "unknown";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "unknown";
  return formatDistanceToNowStrict(d);
}

function CoordinatorLeaseStatusPill({ status, leader }: CoordinatorLeaseStatusPillProps) {
  let className: string;
  let text: string;

  if (status === "active" && leader) {
    className =
      "inline-flex items-center gap-1.5 rounded-full border border-emerald-500/40 bg-emerald-500/10 px-2.5 py-1 text-xs font-medium text-emerald-300";
    text = `AI Coordinator: running (${leader.instanceId.slice(0, 8)}, ${safeFormatAge(leader.renewedAt)})`;
  } else if (status === "stale" && leader) {
    className =
      "inline-flex items-center gap-1.5 rounded-full border border-amber-500/40 bg-amber-500/10 px-2.5 py-1 text-xs font-medium text-amber-300";
    text = `AI Coordinator: stale lease (last heartbeat ${safeFormatAge(leader.renewedAt)} ago)`;
  } else {
    className =
      "inline-flex items-center gap-1.5 rounded-full border border-border/40 bg-muted/20 px-2.5 py-1 text-xs font-medium text-muted-foreground";
    text = "AI Coordinator: vacant";
  }

  return (
    <span
      className={className}
      data-ui-bridge-id="productivity.lease-status-pill"
      data-lease-status={status}
    >
      {text}
    </span>
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
  const [sourceFilter, setSourceFilter] = useState<SourceFilterKey>("all");
  const [highlightedReviewId, setHighlightedReviewId] = useState<string | null>(null);
  const [leaderResponse, setLeaderResponse] = useState<LeaderResponse | null>(null);
  const [launchError, setLaunchError] = useState<string | null>(null);

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

  // Phase 3 — Coordinator lease (header pill + Start button gating). The
  // backend `get_coordinator_leader` Tauri command may not exist on older
  // runner builds; swallow errors and fall back to the `vacant` default.
  const loadLease = useCallback(async () => {
    try {
      const resp = await getCoordinatorLeader();
      setLeaderResponse(resp);
    } catch {
      // Soft-fail: keep the previous response (or null) so the pill renders
      // as `vacant` without flashing an error to the user.
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- async data load on dependency change
    void loadLease();
    const id = setInterval(() => {
      void loadLease();
    }, 5000);
    return () => clearInterval(id);
  }, [loadLease]);

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

  // Phase 3 launch controls. After a successful spawn, immediately refresh
  // the lease so the pill flips to "active" without waiting for the next
  // 5s tick, then hand off to the Terminals tab via the sidebar SM.
  // `navigateHandler` is registered unconditionally by useAppNavigation
  // (see useAppNavigation.ts:368). Calling it with the page slug
  // `"terminal"` flips the active main tab via PAGE_TO_TAB.
  // Avoid `__UI_BRIDGE__.stateMachine.navigateTo` — that property is
  // only present when the spec auto-compile succeeds, and the runner's
  // 47-element duplicate quarantine routinely leaves it undefined,
  // making the optional-chained call silently no-op.
  const revealTerminalTab = useCallback(() => {
    const handler = (
      window as unknown as {
        __UI_BRIDGE__?: { navigateHandler?: (url: string) => void };
      }
    )?.__UI_BRIDGE__?.navigateHandler;
    handler?.("terminal");
  }, []);

  // Workers panel uses this to navigate the user to the Terminals tab
  // when they click "View terminal" on a worker row. Per-pty focus is
  // best-effort: we navigate to the Terminals route and the user picks
  // their worker tab from there. A future iteration could thread
  // `terminalId` through the sidebar SM to auto-activate the matching
  // pty tab; the current target slug `"terminal"` doesn't carry one.
  // Marking the parameter as `_` keeps the contract honest while
  // documenting the intentional drop.
  const handleRevealWorkerTerminal = useCallback(
    (_terminalId: string) => {
      revealTerminalTab();
    },
    [revealTerminalTab],
  );

  // Phase 1.5: launch the Rust scheduler by default. The "rust" mode
  // flips an in-process AtomicBool flag — no pty, no terminal tab to
  // reveal. The legacy "claude_skill" mode (Joshua's debug surface) still
  // spawns a pty and we focus the terminal tab afterwards.
  const handleLaunchCoordinator = useCallback(
    async (mode: CoordinatorLaunchMode = "rust") => {
      setLaunchError(null);
      try {
        const result = await launchCoordinatorSession({
          mode,
          planPath: null,
          titleHint: null,
        });
        await loadLease();
        // Only reveal the terminal tab when a pty was actually opened.
        if (result.terminalId) {
          revealTerminalTab();
        }
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        console.error("Failed to launch Coordinator:", err);
        setLaunchError(`Failed to launch Coordinator: ${msg}`);
      }
    },
    [loadLease, revealTerminalTab],
  );

  const handleStartCoordinatorRust = useCallback(
    () => handleLaunchCoordinator("rust"),
    [handleLaunchCoordinator],
  );
  const handleStartCoordinatorClaudeSkill = useCallback(
    () => handleLaunchCoordinator("claude_skill"),
    [handleLaunchCoordinator],
  );

  const handleStopCoordinator = useCallback(async () => {
    setLaunchError(null);
    try {
      await stopCoordinatorSession();
      await loadLease();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("Failed to stop Coordinator:", err);
      setLaunchError(`Failed to stop Coordinator: ${msg}`);
    }
  }, [loadLease]);

  const handleSpawnWorker = useCallback(async () => {
    setLaunchError(null);
    try {
      await spawnWorkerSession({ titleHint: null });
      await loadLease();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("Failed to spawn worker:", err);
      setLaunchError(`Failed to spawn worker: ${msg}`);
    }
  }, [loadLease]);

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
  // update. Source filter is applied client-side here (no backend filter
  // by session_id today — see SOURCE_FILTER_OPTIONS / §4.5).
  const decisionRows = useMemo(() => {
    if (sourceFilter === "all") return decisions;
    if (sourceFilter === "deconflicter") {
      return decisions.filter((r) => r.sessionId.startsWith(DECONFLICTER_SESSION_PREFIX));
    }
    // "ai": everything that isn't a deconflicter-prefixed row.
    return decisions.filter((r) => !r.sessionId.startsWith(DECONFLICTER_SESSION_PREFIX));
  }, [decisions, sourceFilter]);
  const escalationRows = useMemo(() => escalations, [escalations]);
  const recommendationRows = useMemo(() => recommendations, [recommendations]);
  const advisoryRows = useMemo(() => advisories, [advisories]);

  // Deferrals: derived from the full decision feed (already loaded for the
  // log panel), filtered to Rule B advise-with-text rows whose reasoning
  // body matches one of the two deferral templates. Resolved rows are
  // suppressed — once a force-assign lands or the task transitions out of
  // `ready`, the deconflicter advisory stops appearing.
  const deferralRows = useMemo(
    () => groupDeferrals(decisions.filter((d) => !d.resolved)),
    [decisions],
  );

  const handleForceAssign = useCallback(
    async (taskId: string, sessionId: string) => {
      await dispatchCoordinatorAction(
        {
          type: "assign-task",
          taskId,
          sessionId,
          reasoning: `force-assigned from deferrals panel (task ${taskId} → session ${sessionId})`,
        },
        `[manual] force-assign from deferrals panel`,
      );
      // Refresh the decision feed so the new `rule = "manual"` row
      // shows up alongside the original deferral advisory.
      await loadDecisions();
    },
    [loadDecisions],
  );

  // Defaults to `vacant` when the leader endpoint is unavailable (older
  // runner builds without the Phase 1 backend command).
  const leaseStatus: "active" | "stale" | "vacant" = leaderResponse?.leaseStatus ?? "vacant";
  const leader: CoordinatorLeaderRow | null = leaderResponse?.leader ?? null;

  // Match the existing in-file button styling (see refresh buttons). The
  // dashboard doesn't import a `Button` component — uses plain <button>.
  const baseBtnClass =
    "inline-flex items-center gap-1 rounded-md px-3 py-1.5 text-xs font-medium disabled:opacity-50 disabled:cursor-not-allowed";
  const primaryBtnClass = `${baseBtnClass} bg-primary text-primary-foreground hover:bg-primary/90`;
  const outlineBtnClass = `${baseBtnClass} border border-border text-foreground hover:bg-muted/30`;

  return (
    <div className="h-full overflow-auto bg-background" data-page-id="productivity-coordinator">
      <div className="flex flex-col gap-4 p-4 max-w-5xl mx-auto">
        <div className="flex items-center justify-between mb-2">
          <CoordinatorLeaseStatusPill status={leaseStatus} leader={leader} />
          <div className="flex gap-2">
            {/* Phase 1.5 — primary Start button drives the in-process Rust
                scheduler via mode: "rust". Stop button flips the flag back
                off. The "Force-takeover" label only kicks in when an
                external lease holder has gone stale. */}
            <button
              type="button"
              className={leaseStatus === "active" ? outlineBtnClass : primaryBtnClass}
              disabled={leaseStatus === "active"}
              onClick={handleStartCoordinatorRust}
              data-ui-bridge-id="productivity.launch-coordinator"
            >
              {leaseStatus === "stale" ? "Force-takeover Coordinator" : "Start Coordinator"}
            </button>
            <button
              type="button"
              className={outlineBtnClass}
              disabled={leaseStatus !== "active"}
              onClick={handleStopCoordinator}
              data-ui-bridge-id="productivity.stop-coordinator"
            >
              Stop Coordinator
            </button>
            {/* Debug surface — kept around so Joshua can still spawn the
                Claude pty `/coordinate` session for hand-debugging the
                slash command. Most users never click this. */}
            <button
              type="button"
              className={outlineBtnClass}
              disabled={leaseStatus === "active"}
              onClick={handleStartCoordinatorClaudeSkill}
              data-ui-bridge-id="productivity.launch-coordinator-claude-skill"
              title="Debug: spawn /coordinate Claude pty (legacy)"
            >
              Start (Claude pty)
            </button>
            <button
              type="button"
              className={outlineBtnClass}
              onClick={handleSpawnWorker}
              data-ui-bridge-id="productivity.spawn-worker"
            >
              Spawn worker tab
            </button>
          </div>
        </div>
        {launchError ? (
          <div
            className="rounded-md border border-red-500/30 bg-red-500/10 p-2 text-xs text-red-400"
            data-ui-bridge-id="productivity.launch-error"
            data-error-message={launchError}
          >
            {launchError}
          </div>
        ) : null}

        {/*
         * Workers panel — Phase 6 follow-up R2.
         *
         * Lives ABOVE the productivity-stack-spec-locked panel order
         * block (PlanRecommendations -> Recommendations -> Advisories ->
         * Escalations -> Decision Log) because it's a status display,
         * not a productivity-stack-spec panel. Sits right after the
         * launch buttons so the user gets immediate visual feedback
         * that a freshly-spawned worker is alive.
         */}
        <WorkersPanel onRevealTerminal={handleRevealWorkerTerminal} />

        <header>
          <h1 className="text-lg font-semibold text-foreground">Coordinator dashboard</h1>
          <p className="text-sm text-muted-foreground">
            Cheap-rule decisions auto-act; destructive recommendations (kill-session,
            force-promote-to-worktree) escalate here for your approval.
          </p>
        </header>

        {/*
         * Panel order is part of the productivity-stack spec
         * (productivity-stack.md §6, productivity.spec.uibridge.json
         * group `productivity-coordinator-panels:order-invariant`):
         * PlanRecommendations → Recommendations → Advisories →
         * Escalations → Decision Log. Spec assertions read this
         * order top-to-bottom. Don't reorder without updating the
         * spec assertion in the same PR.
         */}
        <PlanRecommendations />

        {/*
         * Blind-Spot Recommender — mounted directly below
         * PlanRecommendations. It's a cheap-rule recommendation card in
         * the same family as PlanRecommendations (own `GET /blind-spots`
         * endpoint, no LLM), not one of the five productivity-stack
         * spec-locked panels (Recommendations → Advisories → Escalations
         * → Decision Log). The spec's order-invariant asserts the
         * relative top-to-bottom order of those five named panels; an
         * extra card between PlanRecommendations and Recommendations does
         * not change their relative order.
         */}
        <BlindSpotsPanel />

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

        <DeferralsPanel
          rows={deferralRows}
          loading={decisionsLoading}
          error={decisionsError}
          onRefresh={loadDecisions}
          onForceAssign={handleForceAssign}
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
          sourceFilter={sourceFilter}
          onRuleFilterChange={setRuleFilter}
          onActionFilterChange={setActionFilter}
          onSourceFilterChange={setSourceFilter}
          onRefresh={loadDecisions}
        />

        {/*
         * File Activity panel — file-ownership-heatmap-plan Phase 2.
         *
         * Lives BELOW the productivity-stack spec-locked five-panel
         * block (PlanRecommendations → Recommendations → Advisories →
         * Escalations → Decision Log). The spec asserts existence of
         * each, not relative order, so appending here is safe — the
         * description in productivity/spec.uibridge.json is updated in
         * the same PR to keep the human-readable narrative accurate.
         */}
        <FileActivityPanel />

        {/*
         * Fleet Health panel — Row 9 Phase 4. Same placement rationale
         * as FileActivityPanel: below the spec-locked five-panel block,
         * which asserts `exists` per panel (not order). The spec
         * narrative in productivity/spec.uibridge.json gets a parallel
         * one-line update in the same commit.
         */}
        <FleetHealthPanel />
      </div>
    </div>
  );
}

export default CoordinatorDashboard;

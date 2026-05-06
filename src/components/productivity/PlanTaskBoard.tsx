/**
 * PlanTaskBoard
 *
 * Three-column board for inspecting plans and their decomposed tasks.
 *
 *   ┌──────────────┬─────────────────────────────┬───────────────────┐
 *   │ Plans (25%)  │ Tasks for selected plan     │ Task detail (25%) │
 *   │              │ — grouped by phase, sorted  │ claims, deps,     │
 *   │              │   by sequenceInPhase, with  │ status, review,   │
 *   │              │   status-coloured badges    │ notes             │
 *   └──────────────┴─────────────────────────────┴───────────────────┘
 *
 * Phase 1 (Foundation): read-only board. Backend `productivity_*` Tauri
 * commands may not be registered yet — invoke errors are caught and
 * rendered as a soft empty state rather than crashing the page.
 *
 * Mirrors the workflow-queue page conventions for styling: Tailwind
 * utility classes, design tokens (bg-card, border-border, text-foreground),
 * lucide-react icons.
 */

import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import {
  RefreshCw,
  ChevronDown,
  ChevronRight,
  Circle,
  CheckCircle2,
  AlertCircle,
  Loader2,
  PauseCircle,
  Flag,
  XCircle,
  Sparkles,
  Archive,
  BookOpen,
  Undo2,
  ClipboardList,
  ShieldCheck,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { autoReviewTask, getTaskDetail, getPlanTasks, type DecomposeResult } from "./api";
import { listPlansFiltered } from "./reflectionApi";
import { ReflectionPanel } from "./ReflectionPanel";
import { DecomposePlanModal } from "./DecomposePlanModal";
import {
  rewindSession,
  summarizeSession,
  type RewindResult,
  type SummarizeResult,
} from "./sessionApi";
import {
  BriefingPreview,
  CompletionReportSection,
  ManualFireForm,
  UpstreamReportsPreview,
} from "./CompletionReportSections";
import type { PlanRow, PlanStatus, TaskDetail, TaskRow, TaskStatus } from "./types";

/** localStorage key prefix for the per-plan reflection panel open/closed state. */
const REFLECTION_OPEN_KEY = (planId: string) => `productivity.reflection-open.${planId}`;

// ============================================================================
// Status visual mapping
// ============================================================================

interface BadgeStyle {
  /** Tailwind color classes for the badge. */
  classes: string;
  Icon: LucideIcon;
  label: string;
}

const TASK_STATUS_STYLES: Record<TaskStatus, BadgeStyle> = {
  pending: {
    classes: "bg-muted/30 text-muted-foreground border-border",
    Icon: Circle,
    label: "Pending",
  },
  ready: {
    classes: "bg-blue-500/10 text-blue-400 border-blue-500/40",
    Icon: Sparkles,
    label: "Ready",
  },
  assigned: {
    classes: "bg-cyan-500/10 text-cyan-400 border-cyan-500/40",
    Icon: Circle,
    label: "Assigned",
  },
  running: {
    classes: "bg-blue-500/15 text-blue-300 border-blue-500/40",
    Icon: Loader2,
    label: "Running",
  },
  review: {
    classes: "bg-purple-500/10 text-purple-400 border-purple-500/40",
    Icon: PauseCircle,
    label: "In Review",
  },
  needs_fix: {
    classes: "bg-amber-500/10 text-amber-400 border-amber-500/40",
    Icon: AlertCircle,
    label: "Needs Fix",
  },
  done: {
    classes: "bg-emerald-500/10 text-emerald-400 border-emerald-500/40",
    Icon: CheckCircle2,
    label: "Done",
  },
  escalated: {
    classes: "bg-red-500/10 text-red-400 border-red-500/40",
    Icon: Flag,
    label: "Escalated",
  },
  cancelled: {
    classes: "bg-muted/20 text-muted-foreground/70 border-border",
    Icon: XCircle,
    label: "Cancelled",
  },
};

const PLAN_STATUS_STYLES: Record<PlanStatus, BadgeStyle> = {
  draft: {
    classes: "bg-muted/30 text-muted-foreground border-border",
    Icon: Circle,
    label: "Draft",
  },
  vetted: {
    classes: "bg-blue-500/10 text-blue-400 border-blue-500/40",
    Icon: Sparkles,
    label: "Vetted",
  },
  decomposed: {
    classes: "bg-cyan-500/10 text-cyan-400 border-cyan-500/40",
    Icon: CheckCircle2,
    label: "Decomposed",
  },
  implementing: {
    classes: "bg-purple-500/10 text-purple-400 border-purple-500/40",
    Icon: Loader2,
    label: "Implementing",
  },
  done: {
    classes: "bg-emerald-500/10 text-emerald-400 border-emerald-500/40",
    Icon: CheckCircle2,
    label: "Done",
  },
  abandoned: {
    classes: "bg-muted/20 text-muted-foreground/70 border-border",
    Icon: XCircle,
    label: "Abandoned",
  },
};

function StatusBadge({ status }: { status: TaskStatus | PlanStatus }) {
  const style =
    (TASK_STATUS_STYLES as Record<string, BadgeStyle>)[status] ??
    (PLAN_STATUS_STYLES as Record<string, BadgeStyle>)[status];
  if (!style) return null;
  const Icon = style.Icon;
  return (
    <span
      className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium tracking-wide ${style.classes}`}
    >
      <Icon className="w-3 h-3" />
      {style.label}
    </span>
  );
}

// ============================================================================
// Plans column
// ============================================================================

interface PlansListProps {
  plans: PlanRow[];
  selectedPlanId: string | null;
  onSelect: (id: string) => void;
  onRefresh: () => void;
  loading: boolean;
  error: string | null;
  showArchived: boolean;
  onShowArchivedChange: (next: boolean) => void;
  onDecompose: () => void;
}

function PlansList({
  plans,
  selectedPlanId,
  onSelect,
  onRefresh,
  loading,
  error,
  showArchived,
  onShowArchivedChange,
  onDecompose,
}: PlansListProps) {
  return (
    <div
      role="region"
      aria-labelledby="productivity-plan-list-heading"
      className="flex flex-col h-full border-r border-border bg-card/30"
      data-ui-bridge-id="productivity.plan-list"
    >
      <div className="flex items-center justify-between px-3 py-2 border-b border-border">
        <h2 id="productivity-plan-list-heading" className="text-sm font-semibold text-foreground">
          Plans
        </h2>
        <div className="flex items-center gap-2">
          <label
            className="inline-flex items-center gap-1 text-[11px] text-muted-foreground hover:text-foreground cursor-pointer"
            data-ui-bridge-id="productivity.show-archived-toggle"
            title="Include done and abandoned plans"
          >
            <input
              type="checkbox"
              checked={showArchived}
              onChange={(e) => onShowArchivedChange(e.target.checked)}
              className="accent-primary"
            />
            <Archive className="w-3 h-3" />
            archived
          </label>
          <button
            onClick={onDecompose}
            className="inline-flex items-center gap-1 px-2 py-1 rounded text-[11px] font-medium border border-border text-foreground hover:bg-primary/10 hover:text-primary hover:border-primary/40 transition-colors"
            data-ui-bridge-id="productivity.decompose-plan-button"
            title="Decompose a plan markdown into the task graph"
          >
            <ClipboardList className="w-3 h-3" />
            Decompose
          </button>
          <button
            onClick={onRefresh}
            disabled={loading}
            className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors"
            aria-label="Refresh plans"
          >
            <RefreshCw className={`w-3.5 h-3.5 ${loading ? "animate-spin" : ""}`} />
          </button>
        </div>
      </div>
      <div className="flex-1 overflow-y-auto">
        {error ? (
          <div className="p-3 text-xs text-muted-foreground italic">
            Plan registry unavailable. The backend command may not be registered yet.
          </div>
        ) : loading && plans.length === 0 ? (
          <div className="p-3 flex items-center gap-2 text-xs text-muted-foreground">
            <Loader2 className="w-3 h-3 animate-spin" /> Loading plans…
          </div>
        ) : plans.length === 0 ? (
          <div className="p-3 text-xs text-muted-foreground italic">
            No plans yet. Run <code className="font-mono text-[11px]">/decompose-plan</code> to
            populate this list.
          </div>
        ) : (
          <ul className="divide-y divide-border/60">
            {plans.map((plan) => {
              const isActive = plan.id === selectedPlanId;
              return (
                <li key={plan.id}>
                  <button
                    onClick={() => onSelect(plan.id)}
                    data-ui-bridge-id={`productivity.plan-row[${plan.id}]`}
                    className={`w-full text-left px-3 py-2 transition-colors ${
                      isActive
                        ? "bg-primary/10 border-l-2 border-primary"
                        : "hover:bg-muted/30 border-l-2 border-transparent"
                    }`}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span
                        className={`text-sm font-medium truncate ${
                          isActive ? "text-primary" : "text-foreground"
                        }`}
                      >
                        {plan.title ?? plan.markdownPath.split(/[\\/]/).pop() ?? plan.id}
                      </span>
                      <StatusBadge status={plan.status} />
                    </div>
                    {plan.summary && (
                      <p className="mt-1 text-xs text-muted-foreground line-clamp-2">
                        {plan.summary}
                      </p>
                    )}
                    <p className="mt-1 text-[10px] font-mono text-muted-foreground/70 truncate">
                      {plan.markdownPath}
                    </p>
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Tasks column
// ============================================================================

interface TaskRowItemProps {
  task: TaskRow;
  isActive: boolean;
  onSelect: () => void;
}

function TaskRowItem({ task, isActive, onSelect }: TaskRowItemProps) {
  const [expanded, setExpanded] = useState(false);
  const Chevron = expanded ? ChevronDown : ChevronRight;

  return (
    <li
      data-ui-bridge-id={`productivity.task-row[${task.id}]`}
      className={`border-b border-border/60 ${isActive ? "bg-primary/5" : ""}`}
    >
      <div className="flex items-stretch">
        <button
          onClick={() => setExpanded((prev) => !prev)}
          className="px-2 py-2 text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors"
          aria-label={expanded ? "Collapse description" : "Expand description"}
        >
          <Chevron className="w-3 h-3" />
        </button>
        <button
          onClick={onSelect}
          className={`flex-1 text-left px-2 py-2 transition-colors ${
            isActive ? "" : "hover:bg-muted/30"
          }`}
        >
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-mono text-muted-foreground shrink-0">
              #{task.sequenceInPhase}
            </span>
            <span
              className={`flex-1 truncate text-sm ${
                isActive ? "text-primary font-medium" : "text-foreground"
              }`}
            >
              {task.description.split("\n")[0]}
            </span>
            <StatusBadge status={task.status} />
          </div>
        </button>
      </div>
      {expanded && (
        <div className="px-9 pb-3 pt-1 text-xs text-muted-foreground whitespace-pre-wrap">
          {task.description}
        </div>
      )}
    </li>
  );
}

interface TasksListProps {
  plan: PlanRow | null;
  tasks: TaskRow[];
  selectedTaskId: string | null;
  onSelect: (id: string) => void;
  onRefresh: () => void;
  loading: boolean;
  error: string | null;
}

function TasksList({
  plan,
  tasks,
  selectedTaskId,
  onSelect,
  onRefresh,
  loading,
  error,
}: TasksListProps) {
  // Group by phaseName, preserve sequenceInPhase ordering inside each group.
  const grouped = useMemo(() => {
    const buckets = new Map<string, TaskRow[]>();
    for (const task of tasks) {
      const phase = task.phaseName || "Phase";
      const list = buckets.get(phase) ?? [];
      list.push(task);
      buckets.set(phase, list);
    }
    for (const list of buckets.values()) {
      list.sort((a, b) => a.sequenceInPhase - b.sequenceInPhase);
    }
    return Array.from(buckets.entries());
  }, [tasks]);

  return (
    <div
      role="region"
      aria-labelledby="productivity-task-list-heading"
      className="flex flex-col h-full border-r border-border bg-background"
      data-ui-bridge-id="productivity.task-list"
    >
      <div className="flex items-center justify-between px-3 py-2 border-b border-border">
        <h2
          id="productivity-task-list-heading"
          className="text-sm font-semibold text-foreground truncate"
        >
          {plan ? `Tasks · ${plan.title ?? plan.markdownPath}` : "Tasks"}
        </h2>
        <button
          onClick={onRefresh}
          disabled={loading || !plan}
          className="p-1 rounded text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors disabled:opacity-50"
          aria-label="Refresh tasks"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${loading ? "animate-spin" : ""}`} />
        </button>
      </div>
      <div className="flex-1 overflow-y-auto">
        {!plan ? (
          <div className="p-3 text-xs text-muted-foreground italic">
            Select a plan on the left to see its tasks.
          </div>
        ) : error ? (
          <div className="p-3 text-xs text-muted-foreground italic">
            Could not load tasks for this plan.
          </div>
        ) : loading && tasks.length === 0 ? (
          <div className="p-3 flex items-center gap-2 text-xs text-muted-foreground">
            <Loader2 className="w-3 h-3 animate-spin" /> Loading tasks…
          </div>
        ) : tasks.length === 0 ? (
          <div className="p-3 text-xs text-muted-foreground italic">
            No tasks recorded for this plan.
          </div>
        ) : (
          <div className="divide-y divide-border">
            {grouped.map(([phaseName, phaseTasks]) => (
              <section key={phaseName}>
                <h3 className="px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/80 bg-muted/20">
                  {phaseName}
                </h3>
                <ul>
                  {phaseTasks.map((task) => (
                    <TaskRowItem
                      key={task.id}
                      task={task}
                      isActive={task.id === selectedTaskId}
                      onSelect={() => onSelect(task.id)}
                    />
                  ))}
                </ul>
              </section>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Session actions (Phase 5)
// ============================================================================
//
// Surfaces the in-product /summarize-session and /rewind-session features
// for the assigned worker session. Renders Summarize + Rewind buttons
// inside the task detail panel; results are shown inline as a small
// status block (toast-style) that the user can dismiss.

// ============================================================================
// Review Now button — Phase 4 manual auto-review trigger
// ============================================================================
//
// Fires `auto_review_task` against the task currently in detail view. The
// row hits the dashboard's recommendations / ReviewBadge surfaces via the
// `review-completed` event the Tauri command emits — no parent re-fetch
// required from this side. Errors prefixed with "LLM provider not
// configured" render an extra "Settings → AI" hint inline.

interface ReviewNowButtonProps {
  taskId: string;
}

type ReviewNowState =
  | { kind: "idle" }
  | { kind: "running" }
  | { kind: "ok"; verdict: string; confidence: number }
  | { kind: "error"; message: string };

function ReviewNowButton({ taskId }: ReviewNowButtonProps) {
  const [state, setState] = useState<ReviewNowState>({ kind: "idle" });

  const handleClick = useCallback(async () => {
    setState({ kind: "running" });
    try {
      const result = await autoReviewTask(taskId);
      setState({ kind: "ok", verdict: result.verdict, confidence: result.confidence });
    } catch (err) {
      setState({ kind: "error", message: String(err) });
    }
  }, [taskId]);

  const isRunning = state.kind === "running";

  return (
    <div className="flex items-center gap-1.5">
      <button
        type="button"
        onClick={handleClick}
        disabled={isRunning}
        className="inline-flex items-center gap-1 px-1.5 py-0.5 text-[10px] rounded border border-border bg-card hover:bg-muted/30 text-foreground disabled:opacity-50 transition-colors"
        data-ui-bridge-id="productivity.review-now-button"
        title="Run an automated review against the latest worker output"
      >
        {isRunning ? (
          <Loader2 className="w-3 h-3 animate-spin" />
        ) : (
          <ShieldCheck className="w-3 h-3" />
        )}
        Review now
      </button>
      {state.kind === "ok" && (
        <span
          className="text-[10px] text-emerald-300"
          data-ui-bridge-id="productivity.review-now-result"
        >
          {state.verdict} ({(state.confidence * 100).toFixed(0)}%)
        </span>
      )}
      {state.kind === "error" && (
        <span
          className="text-[10px] text-amber-300 truncate max-w-[180px]"
          title={state.message}
          data-ui-bridge-id="productivity.review-now-error"
        >
          {state.message.startsWith("LLM provider not configured")
            ? "LLM not configured"
            : state.message}
        </span>
      )}
    </div>
  );
}

interface SessionActionsProps {
  sessionId: string;
}

type ActionState =
  | { kind: "idle" }
  | { kind: "running"; action: "summarize" | "rewind" }
  | { kind: "summarized"; result: SummarizeResult }
  | { kind: "rewound"; result: RewindResult }
  | { kind: "error"; message: string };

function SessionActions({ sessionId }: SessionActionsProps) {
  const [state, setState] = useState<ActionState>({ kind: "idle" });
  const [confirmRewind, setConfirmRewind] = useState(false);

  const handleSummarize = useCallback(async () => {
    setState({ kind: "running", action: "summarize" });
    try {
      const result = await summarizeSession(sessionId);
      setState({ kind: "summarized", result });
    } catch (err) {
      setState({ kind: "error", message: String(err) });
    }
  }, [sessionId]);

  const handleRewind = useCallback(
    async (noReplay: boolean) => {
      setConfirmRewind(false);
      setState({ kind: "running", action: "rewind" });
      try {
        const result = await rewindSession(sessionId, noReplay);
        setState({ kind: "rewound", result });
      } catch (err) {
        setState({ kind: "error", message: String(err) });
      }
    },
    [sessionId],
  );

  const isRunning = state.kind === "running";

  return (
    <section data-ui-bridge-id="productivity.session-actions">
      <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
        Session actions
      </h3>
      <div className="mt-2 flex gap-2">
        <button
          onClick={handleSummarize}
          disabled={isRunning}
          className="inline-flex items-center gap-1.5 px-2 py-1 text-[11px] rounded border border-border bg-card hover:bg-muted/30 text-foreground disabled:opacity-50 transition-colors"
          data-ui-bridge-id="productivity.summarize-button"
        >
          {state.kind === "running" && state.action === "summarize" ? (
            <Loader2 className="w-3 h-3 animate-spin" />
          ) : (
            <BookOpen className="w-3 h-3" />
          )}
          Summarize
        </button>
        <button
          onClick={() => setConfirmRewind(true)}
          disabled={isRunning}
          className="inline-flex items-center gap-1.5 px-2 py-1 text-[11px] rounded border border-amber-700/50 bg-amber-950/20 hover:bg-amber-900/30 text-amber-200 disabled:opacity-50 transition-colors"
          data-ui-bridge-id="productivity.rewind-button"
        >
          {state.kind === "running" && state.action === "rewind" ? (
            <Loader2 className="w-3 h-3 animate-spin" />
          ) : (
            <Undo2 className="w-3 h-3" />
          )}
          Rewind
        </button>
      </div>

      {confirmRewind && (
        <div
          className="mt-2 p-2 rounded border border-amber-700/50 bg-amber-950/30 text-[11px] space-y-2"
          data-ui-bridge-id="productivity.rewind-confirm"
          role="alertdialog"
          aria-labelledby="productivity-rewind-confirm-heading"
        >
          <p
            id="productivity-rewind-confirm-heading"
            className="font-medium text-amber-200"
          >
            Rewind session {sessionId.slice(0, 8)}…?
          </p>
          <p className="text-amber-300/90">
            This restores files from the pre-edit snapshots, kills the worker, and (by default) spawns a replacement with failure context. Destructive — files modified by this session will be reverted.
          </p>
          <div className="flex gap-2">
            <button
              onClick={() => handleRewind(false)}
              className="px-2 py-1 rounded bg-amber-700 hover:bg-amber-600 text-amber-50 transition-colors"
              data-ui-bridge-id="productivity.rewind-confirm-replay"
            >
              Rewind + Replay
            </button>
            <button
              onClick={() => handleRewind(true)}
              className="px-2 py-1 rounded bg-amber-900/50 hover:bg-amber-800 text-amber-100 transition-colors"
              data-ui-bridge-id="productivity.rewind-confirm-no-replay"
            >
              Rewind only
            </button>
            <button
              onClick={() => setConfirmRewind(false)}
              className="px-2 py-1 rounded bg-muted/30 hover:bg-muted/50 text-muted-foreground transition-colors"
              data-ui-bridge-id="productivity.rewind-confirm-cancel"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {(state.kind === "summarized" ||
        state.kind === "rewound" ||
        state.kind === "error") && (
        <SessionActionResult state={state} onDismiss={() => setState({ kind: "idle" })} />
      )}
    </section>
  );
}

interface SessionActionResultProps {
  state: ActionState;
  onDismiss: () => void;
}

function SessionActionResult({ state, onDismiss }: SessionActionResultProps) {
  let body: ReactNode = null;
  let bgClass = "border-border bg-muted/20 text-foreground";
  let testId = "";

  if (state.kind === "summarized") {
    const { result } = state;
    bgClass = result.placeholder
      ? "border-amber-700/50 bg-amber-950/20 text-amber-200"
      : "border-emerald-700/50 bg-emerald-950/20 text-emerald-200";
    testId = "productivity.summarize-result";
    body = (
      <>
        <div className="font-medium">
          {result.placeholder
            ? "Placeholder summary inserted (no LLM provider configured)"
            : `Summarized ${result.learningCount} learning(s) — verdict: ${result.verdict}`}
        </div>
        {!result.placeholder && Object.keys(result.byArea).length > 0 && (
          <ul className="mt-1 space-y-0.5 text-[11px]">
            {Object.entries(result.byArea)
              .sort((a, b) => a[0].localeCompare(b[0]))
              .map(([area, count]) => (
                <li key={area} className="font-mono">
                  {area}: {count}
                </li>
              ))}
          </ul>
        )}
        {result.placeholder && (
          <p className="mt-1 text-[11px]">
            Configure an LLM provider in Settings → AI to extract real learnings.
          </p>
        )}
      </>
    );
  } else if (state.kind === "rewound") {
    const { result } = state;
    bgClass = "border-emerald-700/50 bg-emerald-950/20 text-emerald-200";
    testId = "productivity.rewind-result";
    body = (
      <>
        <div className="font-medium">Rewound session</div>
        <ul className="mt-1 space-y-0.5 text-[11px]">
          <li>Files restored: {result.filesRestored}</li>
          <li>Files skipped: {result.filesSkipped}</li>
          <li>
            Replay session:{" "}
            {result.replaySessionId == null ? (
              <span className="italic">none (no-replay)</span>
            ) : (
              <span className="font-mono">{result.replaySessionId.slice(0, 16)}…</span>
            )}
          </li>
          {result.summarized && result.verdict && (
            <li>
              Verdict captured: <span className="font-mono">{result.verdict}</span>
            </li>
          )}
        </ul>
      </>
    );
  } else if (state.kind === "error") {
    bgClass = "border-rose-700/50 bg-rose-950/20 text-rose-200";
    testId = "productivity.session-action-error";
    body = (
      <>
        <div className="font-medium">Action failed</div>
        <p className="mt-1 whitespace-pre-wrap text-[11px]">{state.message}</p>
      </>
    );
  }

  if (body == null) return null;

  return (
    <div
      className={`mt-2 p-2 rounded border text-[11px] ${bgClass}`}
      data-ui-bridge-id={testId}
    >
      {body}
      <button
        onClick={onDismiss}
        className="mt-2 text-[10px] underline opacity-80 hover:opacity-100"
        data-ui-bridge-id="productivity.session-action-dismiss"
      >
        Dismiss
      </button>
    </div>
  );
}

// ============================================================================
// Task detail column
// ============================================================================

interface TaskDetailPanelProps {
  detail: TaskDetail | null;
  loading: boolean;
  error: string | null;
  /** Triggered after a manual-fire (or any other in-panel action that
   *  mutates the task) so the parent can re-fetch task detail + tasks list. */
  onReloadDetail?: () => void;
}

function TaskDetailPanel({ detail, loading, error, onReloadDetail }: TaskDetailPanelProps) {
  return (
    <div
      role="region"
      aria-labelledby="productivity-task-detail-heading"
      className="flex flex-col h-full bg-card/20"
      data-ui-bridge-id="productivity.task-detail"
    >
      <div className="px-3 py-2 border-b border-border">
        <h2
          id="productivity-task-detail-heading"
          className="text-sm font-semibold text-foreground"
        >
          Task Detail
        </h2>
      </div>
      <div className="flex-1 overflow-y-auto p-3 space-y-4">
        {!detail && !loading && !error ? (
          <p className="text-xs text-muted-foreground italic">
            Select a task to see its claims, dependencies, and review history.
          </p>
        ) : error ? (
          <p className="text-xs text-muted-foreground italic">Could not load task detail.</p>
        ) : loading || !detail ? (
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <Loader2 className="w-3 h-3 animate-spin" /> Loading detail…
          </div>
        ) : (
          <>
            <section>
              <div className="flex items-center justify-between gap-2">
                <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                  Status
                </h3>
                <StatusBadge status={detail.task.status} />
              </div>
              <dl className="mt-2 grid grid-cols-2 gap-x-2 gap-y-1 text-[11px]">
                <dt className="text-muted-foreground">Phase</dt>
                <dd className="text-foreground truncate">{detail.task.phaseName}</dd>
                <dt className="text-muted-foreground">Sequence</dt>
                <dd className="text-foreground">#{detail.task.sequenceInPhase}</dd>
                <dt className="text-muted-foreground">Assigned</dt>
                <dd className="text-foreground truncate">{detail.task.assignedSessionId ?? "—"}</dd>
                <dt className="text-muted-foreground">Started</dt>
                <dd className="text-foreground truncate">{detail.task.startedAt ?? "—"}</dd>
                <dt className="text-muted-foreground">Completed</dt>
                <dd className="text-foreground truncate">{detail.task.completedAt ?? "—"}</dd>
              </dl>
            </section>

            <section data-ui-bridge-id="productivity.task-claims">
              <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Expected file claims
              </h3>
              {detail.task.expectedFileClaims.length === 0 &&
              detail.task.expectedDirs.length === 0 ? (
                <p className="mt-1 text-xs text-muted-foreground/70 italic">None.</p>
              ) : (
                <ul className="mt-1 space-y-1 text-[11px]">
                  {detail.task.expectedFileClaims.map((path) => {
                    const conflicting = (detail.claimersByPath[path] ?? []).filter(
                      (c) => c.taskId !== detail.task.id,
                    ).length;
                    return (
                      <li
                        key={`f:${path}`}
                        className="flex items-start gap-2 font-mono text-foreground"
                      >
                        <span className="truncate flex-1">{path}</span>
                        {conflicting > 0 && (
                          <span className="shrink-0 inline-flex items-center gap-0.5 text-amber-400">
                            <AlertCircle className="w-3 h-3" />
                            {conflicting}
                          </span>
                        )}
                      </li>
                    );
                  })}
                  {detail.task.expectedDirs.map((path) => (
                    <li
                      key={`d:${path}`}
                      className="font-mono text-muted-foreground"
                      title="Directory claim"
                    >
                      {path}/
                    </li>
                  ))}
                </ul>
              )}
            </section>

            <section data-ui-bridge-id="productivity.task-deps">
              <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Depends on
              </h3>
              {detail.task.dependsOn.length === 0 ? (
                <p className="mt-1 text-xs text-muted-foreground/70 italic">None.</p>
              ) : (
                <ul className="mt-1 space-y-0.5 text-[11px] font-mono">
                  {detail.task.dependsOn.map((dep) => (
                    <li key={dep} className="text-foreground truncate">
                      {dep}
                    </li>
                  ))}
                </ul>
              )}
            </section>

            <section data-ui-bridge-id="productivity.task-review">
              <div className="flex items-center justify-between gap-2">
                <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                  Latest review summary
                </h3>
                <ReviewNowButton taskId={detail.task.id} />
              </div>
              <p className="mt-1 text-xs text-muted-foreground">
                {detail.latestReviewSummary ?? "—"}
              </p>
            </section>

            <section>
              <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                Worker session
              </h3>
              <p className="mt-1 text-xs text-muted-foreground">
                {detail.workerSessionMeta == null ? "—" : JSON.stringify(detail.workerSessionMeta)}
              </p>
            </section>

            {detail.task.assignedSessionId && (
              <SessionActions sessionId={detail.task.assignedSessionId} />
            )}

            {detail.task.notes && (
              <section>
                <h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
                  Notes
                </h3>
                <p className="mt-1 text-xs text-foreground whitespace-pre-wrap">
                  {detail.task.notes}
                </p>
              </section>
            )}

            {/* Phase 4 of productivity-coordinator-completion-reports §5 —
             *  structured completion report rendering, upstream-report preview
             *  for pending tasks, briefing preview for assigned/ready+ tasks,
             *  and the manual-fire form. */}
            <CompletionReportSection detail={detail} />
            <UpstreamReportsPreview detail={detail} />
            <BriefingPreview detail={detail} />
            <ManualFireForm detail={detail} onFired={onReloadDetail} />
          </>
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Top-level board
// ============================================================================

export function PlanTaskBoard() {
  const [plans, setPlans] = useState<PlanRow[]>([]);
  const [plansLoading, setPlansLoading] = useState(true);
  const [plansError, setPlansError] = useState<string | null>(null);
  const [showArchived, setShowArchived] = useState<boolean>(false);

  const [selectedPlanId, setSelectedPlanId] = useState<string | null>(null);

  const [tasks, setTasks] = useState<TaskRow[]>([]);
  const [tasksLoading, setTasksLoading] = useState(false);
  const [tasksError, setTasksError] = useState<string | null>(null);

  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [taskDetail, setTaskDetail] = useState<TaskDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState<string | null>(null);

  const [reflectionOpen, setReflectionOpen] = useState<boolean>(false);

  // Decompose-plan modal state. `decomposeInitialPath` is seeded from the
  // currently-selected plan (so re-decomposing an existing plan is a
  // one-click operation). When no plan is selected, the modal opens
  // empty and the user types/pastes a path manually. A future revision
  // could scan `D:/qontinui-root/plans/` for the most-recently-modified
  // *.md, but that requires a Tauri command we don't have today.
  const [decomposeOpen, setDecomposeOpen] = useState<boolean>(false);
  const [decomposeInitialPath, setDecomposeInitialPath] = useState<string | null>(null);

  const fetchPlans = useCallback(async () => {
    setPlansLoading(true);
    setPlansError(null);
    try {
      const rows = await listPlansFiltered(showArchived);
      setPlans(rows);
      // If the previously selected plan vanished or none was selected yet,
      // pick the first row by default.
      setSelectedPlanId((current) => {
        if (current && rows.some((p) => p.id === current)) return current;
        return rows[0]?.id ?? null;
      });
    } catch (err) {
      // Backend command may not be registered in Phase 1 — treat as empty.
      setPlans([]);
      setPlansError(String(err));
      console.warn("[productivity] list_plans_filtered failed (treating as empty):", err);
    } finally {
      setPlansLoading(false);
    }
  }, [showArchived]);

  const fetchTasks = useCallback(async (planId: string) => {
    setTasksLoading(true);
    setTasksError(null);
    try {
      const rows = await getPlanTasks(planId);
      setTasks(rows);
    } catch (err) {
      setTasks([]);
      setTasksError(String(err));
      console.warn("[productivity] get_plan_tasks failed:", err);
    } finally {
      setTasksLoading(false);
    }
  }, []);

  const fetchDetail = useCallback(async (taskId: string) => {
    setDetailLoading(true);
    setDetailError(null);
    try {
      const detail = await getTaskDetail(taskId);
      setTaskDetail(detail);
    } catch (err) {
      setTaskDetail(null);
      setDetailError(String(err));
      console.warn("[productivity] get_task_detail failed:", err);
    } finally {
      setDetailLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    // Defer via microtask so the effect body itself doesn't synchronously
    // trigger setState inside fetchPlans (matches the workflow-queue pattern).
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchPlans();
    });
    return () => {
      cancelled = true;
    };
  }, [fetchPlans]);

  useEffect(() => {
    if (!selectedPlanId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- clearing dependent state when the parent selection is cleared
      setTasks([]);
      setSelectedTaskId(null);
      return;
    }
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchTasks(selectedPlanId);
    });
    return () => {
      cancelled = true;
    };
  }, [selectedPlanId, fetchTasks]);

  useEffect(() => {
    if (!selectedTaskId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- clearing dependent state when the parent selection is cleared
      setTaskDetail(null);
      return;
    }
    let cancelled = false;
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchDetail(selectedTaskId);
    });
    return () => {
      cancelled = true;
    };
  }, [selectedTaskId, fetchDetail]);

  // Read the persisted reflection-panel open/closed state when the
  // selected plan changes. Default closed when no record exists.
  useEffect(() => {
    if (!selectedPlanId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- mirror selection state into UI defaults
      setReflectionOpen(false);
      return;
    }
    try {
      const raw = window.localStorage.getItem(REFLECTION_OPEN_KEY(selectedPlanId));
      setReflectionOpen(raw === "1");
    } catch {
      setReflectionOpen(false);
    }
  }, [selectedPlanId]);

  const handleReflectionToggle = useCallback(() => {
    setReflectionOpen((prev) => {
      const next = !prev;
      if (selectedPlanId) {
        try {
          window.localStorage.setItem(REFLECTION_OPEN_KEY(selectedPlanId), next ? "1" : "0");
        } catch {
          /* localStorage unavailable; in-memory only */
        }
      }
      return next;
    });
  }, [selectedPlanId]);

  // Listen for cross-component plan-selection events (e.g. from
  // PlanRecommendations on the Coordinator dashboard).
  useEffect(() => {
    function onSelectPlan(e: Event) {
      const detail = (e as CustomEvent<{ planId?: string }>).detail;
      if (detail?.planId) {
        setSelectedPlanId(detail.planId);
      }
    }
    window.addEventListener("productivity-select-plan", onSelectPlan);
    return () => window.removeEventListener("productivity-select-plan", onSelectPlan);
  }, []);

  // Listen for cross-component task-selection events (e.g. from the
  // Recommendations queue's "Open task" button on the Coordinator
  // dashboard). The task may belong to a plan that isn't yet selected,
  // so we fetch the task detail to discover its parent plan, switch to
  // that plan, then highlight the task.
  useEffect(() => {
    function onSelectTask(e: Event) {
      const detail = (e as CustomEvent<{ taskId?: string }>).detail;
      const taskId = detail?.taskId;
      if (!taskId) return;
      let cancelled = false;
      void (async () => {
        try {
          const td = await getTaskDetail(taskId);
          if (cancelled) return;
          setSelectedPlanId(td.task.planId);
          setSelectedTaskId(taskId);
        } catch {
          // Best-effort: silently ignore. The Coordinator dashboard
          // already navigated to the Plans sub-view; user can pick
          // the task by hand.
        }
      })();
      return () => {
        cancelled = true;
      };
    }
    window.addEventListener("productivity-select-task", onSelectTask);
    return () => window.removeEventListener("productivity-select-task", onSelectTask);
  }, []);

  const selectedPlan = useMemo(
    () => plans.find((p) => p.id === selectedPlanId) ?? null,
    [plans, selectedPlanId],
  );

  const handleOpenDecompose = useCallback(() => {
    // Seed with the currently-selected plan's path so re-decomposing is
    // one-click. The modal happily accepts an empty path otherwise.
    setDecomposeInitialPath(selectedPlan?.markdownPath ?? null);
    setDecomposeOpen(true);
  }, [selectedPlan]);

  const handleDecomposeSuccess = useCallback(
    (result: DecomposeResult) => {
      // Refresh the plan list so the freshly-decomposed plan shows up,
      // then surface it as the active selection.
      void fetchPlans();
      setSelectedPlanId(result.planId);
    },
    [fetchPlans],
  );

  return (
    <div className="h-full flex" data-page-id="productivity-plans">
      {/* Left: Plans (25%) */}
      <div className="shrink-0" style={{ width: "25%", minWidth: 220 }}>
        <PlansList
          plans={plans}
          selectedPlanId={selectedPlanId}
          onSelect={setSelectedPlanId}
          onRefresh={fetchPlans}
          loading={plansLoading}
          error={plansError}
          showArchived={showArchived}
          onShowArchivedChange={setShowArchived}
          onDecompose={handleOpenDecompose}
        />
      </div>

      {/* Middle: Reflection panel (collapsible) + Tasks list */}
      <div className="flex-1 min-w-0 flex flex-col">
        {selectedPlan && (
          <div className="border-b border-border bg-background/40">
            <button
              type="button"
              onClick={handleReflectionToggle}
              data-ui-bridge-id="productivity.reflection-toggle"
              data-plan-id={selectedPlan.id}
              className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors"
              aria-expanded={reflectionOpen}
            >
              {reflectionOpen ? (
                <ChevronDown className="w-3 h-3" />
              ) : (
                <ChevronRight className="w-3 h-3" />
              )}
              <Sparkles className="w-3 h-3 text-purple-400" />
              <span className="font-semibold uppercase tracking-wider">Reflection</span>
            </button>
            {reflectionOpen && (
              <div className="p-3">
                <ReflectionPanel planId={selectedPlan.id} plan={selectedPlan} />
              </div>
            )}
          </div>
        )}
        <div className="flex-1 min-h-0">
          <TasksList
            plan={selectedPlan}
            tasks={tasks}
            selectedTaskId={selectedTaskId}
            onSelect={setSelectedTaskId}
            onRefresh={() => selectedPlanId && fetchTasks(selectedPlanId)}
            loading={tasksLoading}
            error={tasksError}
          />
        </div>
      </div>

      {/* Right: Task detail (25%) */}
      <div className="shrink-0" style={{ width: "25%", minWidth: 260 }}>
        <TaskDetailPanel
          detail={taskDetail}
          loading={detailLoading}
          error={detailError}
          onReloadDetail={() => {
            if (selectedTaskId) void fetchDetail(selectedTaskId);
            if (selectedPlanId) void fetchTasks(selectedPlanId);
          }}
        />
      </div>

      {/* Decompose-plan modal — Phase 3 of productivity-stack-product-readiness. */}
      <DecomposePlanModal
        open={decomposeOpen}
        onClose={() => setDecomposeOpen(false)}
        initialPlanPath={decomposeInitialPath}
        onSuccess={handleDecomposeSuccess}
      />
    </div>
  );
}

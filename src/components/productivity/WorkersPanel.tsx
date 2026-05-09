/**
 * WorkersPanel — Phase 6 follow-up R2.
 *
 * Surfaces every live SessionManager session (ClaudeSessions and
 * pty-backed Workers) on the Coordinator dashboard. Polls
 * `GET /coordinator/state` every 5 seconds and renders one row per
 * session with:
 *
 *   - title (or short task_run_id) + state pill
 *   - "task X — short description" line when Processing AND the task
 *     has been pulled from the snapshot's `tasks` collection
 *   - "View terminal" button (pty-backed Workers only) that navigates to
 *     the Terminals tab via the existing UI Bridge navigateHandler hook
 *
 * Sits ABOVE the productivity-stack-spec-locked panel order block (the
 * `productivity-coordinator-panels:order-invariant` group:
 * PlanRecommendations -> Recommendations -> Advisories -> Escalations ->
 * Decision Log). The spec doesn't enumerate this panel; the new
 * data-ui-bridge-id values will appear as orphan elements until the spec
 * is updated separately.
 *
 * Polling cadence is intentionally 5s — multiple agents may share this
 * runner, and the underlying endpoint runs three PG queries plus a
 * SessionManager walk per call.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { Users, RefreshCw, ExternalLink } from "lucide-react";
import { getCoordinatorState, type LiveSession } from "./coordinatorApi";

const POLL_INTERVAL_MS = 5_000;

interface WorkersPanelProps {
  /**
   * Called when the user clicks "View terminal" on a worker row.
   * Receives the pty `terminalId` of the worker. Implementations are
   * expected to navigate to the Terminals tab; per-tab focus is
   * out-of-scope for this panel.
   */
  onRevealTerminal: (terminalId: string) => void;
}

interface AssignedTaskInfo {
  id: string;
  description: string;
}

/** Trim a task_run_id (UUIDv4) to its first segment for display. */
function shortTaskRunId(id: string): string {
  return id.length > 8 ? `${id.slice(0, 8)}...` : id;
}

/** Trim a task id (UUIDv4) to its first segment. */
function shortTaskId(id: string): string {
  return id.length > 8 ? id.slice(0, 8) : id;
}

/** Truncate a task description to a single 50-char preview. */
function truncateDescription(d: string): string {
  const flat = d.replace(/\s+/g, " ").trim();
  return flat.length > 50 ? `${flat.slice(0, 50)}...` : flat;
}

/** Map session state to a Tailwind pill class + label. */
function stateStyle(state: string): { className: string; label: string } {
  const normalized = state.toLowerCase();
  // Workers idle in `ready`; busy in `processing`. ClaudeSessions can
  // also be `created` / `initializing` early on — treat those as
  // neutral. `closed` and `closing` are gray; everything else gets the
  // muted style.
  if (normalized === "ready") {
    return {
      className:
        "border-emerald-500/40 bg-emerald-500/10 text-emerald-300",
      label: "ready",
    };
  }
  if (normalized === "processing") {
    return {
      className: "border-amber-500/40 bg-amber-500/10 text-amber-300",
      label: "processing",
    };
  }
  if (normalized === "closed" || normalized === "closing") {
    return {
      className: "border-border/40 bg-muted/20 text-muted-foreground",
      label: normalized,
    };
  }
  return {
    className: "border-border/40 bg-muted/20 text-muted-foreground",
    label: normalized,
  };
}

export function WorkersPanel({ onRevealTerminal }: WorkersPanelProps) {
  const [sessions, setSessions] = useState<LiveSession[]>([]);
  const [taskById, setTaskById] = useState<Map<string, AssignedTaskInfo>>(
    new Map(),
  );
  const [assignedTaskBySession, setAssignedTaskBySession] = useState<
    Map<string, string>
  >(new Map());
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // `hasLoadedOnce` lets us render the empty state vs. a spinner on the
  // first paint without flashing "no workers" before the first response.
  const [hasLoadedOnce, setHasLoadedOnce] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const snapshot = await getCoordinatorState();
      setSessions(snapshot.liveSessions ?? []);
      const tasks = snapshot.tasks ?? [];
      const tMap = new Map<string, AssignedTaskInfo>();
      const sMap = new Map<string, string>();
      for (const t of tasks) {
        tMap.set(t.id, { id: t.id, description: t.description });
        if (t.assignedSessionId) {
          sMap.set(t.assignedSessionId, t.id);
        }
      }
      setTaskById(tMap);
      setAssignedTaskBySession(sMap);
      setError(null);
    } catch (err) {
      // Render last-good data with a small inline error — mirrors the
      // existing dashboard panels' error UX.
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setHasLoadedOnce(true);
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const id = window.setInterval(() => {
      void load();
    }, POLL_INTERVAL_MS);
    return () => {
      window.clearInterval(id);
    };
  }, [load]);

  // Sort: active first (ready/processing), then by title/id alphabetically
  // so a freshly-spawned worker doesn't displace existing rows on every
  // poll.
  const sortedRows = useMemo(() => {
    const rows = [...sessions];
    rows.sort((a, b) => {
      if (a.isActive !== b.isActive) {
        return a.isActive ? -1 : 1;
      }
      const an = a.title ?? a.taskRunId;
      const bn = b.title ?? b.taskRunId;
      return an.localeCompare(bn);
    });
    return rows;
  }, [sessions]);

  return (
    <section
      role="region"
      aria-labelledby="productivity-workers-heading"
      className="flex flex-col rounded-lg border border-border bg-card/30 p-4 gap-3"
      data-ui-bridge-id="productivity.workers-panel"
    >
      <header className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Users className="w-4 h-4 text-muted-foreground" />
          <h2
            id="productivity-workers-heading"
            className="text-sm font-semibold text-foreground"
          >
            Workers
          </h2>
          <span className="text-xs text-muted-foreground">
            {sortedRows.length} active
          </span>
        </div>
        <button
          type="button"
          onClick={() => void load()}
          disabled={loading}
          className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 disabled:opacity-50"
        >
          <RefreshCw className={`w-3 h-3 ${loading ? "animate-spin" : ""}`} />
          Refresh
        </button>
      </header>

      {error ? (
        <div className="rounded-md border border-red-500/30 bg-red-500/10 p-2 text-xs text-red-400">
          {error}
        </div>
      ) : null}

      {hasLoadedOnce && sortedRows.length === 0 ? (
        <div className="rounded-md border border-border/40 bg-muted/10 p-3 text-xs text-muted-foreground">
          No workers spawned. Click &quot;Spawn worker tab&quot; above to add
          one.
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {sortedRows.map((row) => {
            const assignedTaskId = assignedTaskBySession.get(row.taskRunId);
            const assignedTask = assignedTaskId
              ? taskById.get(assignedTaskId)
              : undefined;
            const showAssignedTask =
              row.state.toLowerCase() === "processing" && assignedTask;
            const pill = stateStyle(row.state);
            const displayName = row.title ?? shortTaskRunId(row.taskRunId);
            const canViewTerminal = Boolean(row.terminalId);
            return (
              <li
                key={row.taskRunId}
                className="rounded-md border border-border/40 bg-background/40 p-3"
                data-ui-bridge-id={`productivity.worker-row[${row.taskRunId}]`}
              >
                <div className="flex items-start justify-between gap-3">
                  <div className="flex flex-col gap-1 min-w-0">
                    <div className="flex items-center gap-2 text-xs">
                      <span className="font-medium text-foreground truncate">
                        {displayName}
                      </span>
                      <span
                        className={`shrink-0 rounded-full border px-2 py-0.5 text-[10px] font-medium ${pill.className}`}
                      >
                        {pill.label}
                      </span>
                    </div>
                    <div className="text-[11px] text-muted-foreground font-mono truncate">
                      {row.taskRunId}
                    </div>
                    {showAssignedTask && assignedTask ? (
                      <div className="text-xs text-foreground/80 truncate">
                        task {shortTaskId(assignedTask.id)} &middot;{" "}
                        {truncateDescription(assignedTask.description)}
                      </div>
                    ) : null}
                  </div>
                  <button
                    type="button"
                    disabled={!canViewTerminal}
                    onClick={() => {
                      if (row.terminalId) {
                        onRevealTerminal(row.terminalId);
                      }
                    }}
                    title={
                      canViewTerminal
                        ? "Open the Terminals tab"
                        : "This session is not pty-backed"
                    }
                    data-ui-bridge-id={`productivity.worker-view-terminal[${row.taskRunId}]`}
                    className="inline-flex items-center gap-1 rounded-md border border-border bg-muted/20 px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/40 disabled:opacity-50 disabled:cursor-not-allowed shrink-0"
                  >
                    <ExternalLink className="w-3 h-3" /> View terminal
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

export default WorkersPanel;

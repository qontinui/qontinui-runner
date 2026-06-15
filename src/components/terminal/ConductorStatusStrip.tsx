/**
 * Approach-D Conductor — Phase 5 conductor status strip.
 *
 * Given a conductor `runId` (== the Terminal page id workers land on), polls
 * `GET /orchestration/runs/{id}` (via the `orchestration_run_status` Tauri
 * command) and surfaces, in one thin strip mirroring {@link StatusStrip}'s
 * idiom:
 *
 *   - the live `LoopPhase` (e.g. planning / running_workflow / complete),
 *   - a per-subtask state summary (counts by state),
 *   - a compact DAG row — one chip per subtask, colored by state, with
 *     dependency arrows so the org-chart's shape is legible at a glance.
 *
 * Data fetch mirrors `useAiSession`/`StatusStrip`'s conventions: a polling
 * `setInterval` over an `invoke<…>` call, refs to avoid stale closures, and
 * a clean unmount teardown. The strip self-hides (`return null`) until the
 * first successful fetch so it never flashes empty chrome — same posture as
 * `StatusStrip`'s auto-hide.
 *
 * Mount: rendered on the run's page (see `TerminalPage.tsx`) keyed by the
 * active page id when that page id looks like a run (a uuid that resolves to
 * a live run). The component is defensive — a non-run page id simply yields a
 * not-found error and the strip stays hidden.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Activity, GitBranch, Loader2 } from "lucide-react";

/** Subtask states (serde snake_case — see `ledger::SubtaskState`). */
type SubtaskState =
  | "submitted"
  | "working"
  | "input_required"
  | "completed"
  | "failed"
  | "canceled";

/** One subtask row (serde snake_case — see `ledger::Subtask`). */
interface Subtask {
  task_id: string;
  run_id: string;
  idx: number;
  title: string;
  phase: string;
  depends_on: string[];
  state: SubtaskState;
  task_run_id: string | null;
}

/** Run row (serde snake_case — see `ledger::Run`). */
interface Run {
  run_id: string;
  goal: string;
  recipe: string | null;
  phases: string[];
  status: string;
}

/** `OrchestrationRunStatus` payload (serde default snake_case). */
interface RunStatus {
  run: Run;
  subtasks: Subtask[];
  loop_phase: string;
  loop_running: boolean;
  error: string | null;
  tick: number;
}

const POLL_MS = 2_000;

/** Per-state color palette — aligns with `StatusStrip`'s STATE_COLORS plus
 *  the conductor-only `submitted` / `canceled` / `input_required` states. */
const STATE_COLORS: Record<SubtaskState, string> = {
  submitted: "#565f89",
  working: "#7aa2f7",
  input_required: "#e0af68",
  completed: "#9ece6a",
  failed: "#f7768e",
  canceled: "#414868",
};

/** Short human label per subtask state for the count pills. */
const STATE_LABELS: Record<SubtaskState, string> = {
  submitted: "queued",
  working: "working",
  input_required: "input",
  completed: "done",
  failed: "failed",
  canceled: "canceled",
};

/** Human-readable phase label from the snake_case `LoopPhase` wire value. */
function phaseLabel(phase: string): string {
  return phase
    .split("_")
    .map((w) => (w ? w[0].toUpperCase() + w.slice(1) : w))
    .join(" ");
}

/** Tally subtasks by state. Exported for unit testing. */
export function countByState(subtasks: Subtask[]): Record<SubtaskState, number> {
  const counts: Record<SubtaskState, number> = {
    submitted: 0,
    working: 0,
    input_required: 0,
    completed: 0,
    failed: 0,
    canceled: 0,
  };
  for (const s of subtasks) {
    counts[s.state] = (counts[s.state] ?? 0) + 1;
  }
  return counts;
}

interface ConductorStatusStripProps {
  /** The conductor run id (== the page id workers land on). */
  runId: string;
}

export function ConductorStatusStrip({ runId }: ConductorStatusStripProps) {
  const [status, setStatus] = useState<RunStatus | null>(null);
  const [notFound, setNotFound] = useState(false);
  const runIdRef = useRef(runId);
  runIdRef.current = runId;

  const poll = useCallback(async () => {
    const id = runIdRef.current;
    if (!id) return;
    try {
      const result = await invoke<RunStatus>("orchestration_run_status", { runId: id });
      // Guard against a late response for a stale run id after navigation.
      if (runIdRef.current !== id) return;
      setStatus(result);
      setNotFound(false);
    } catch {
      if (runIdRef.current !== id) return;
      // No such run (e.g. the active page is a plain terminal page, not a
      // conductor run). Mark not-found so the strip stays hidden.
      setNotFound(true);
    }
  }, []);

  useEffect(() => {
    // Reset on run change so the strip doesn't show a stale run's chips.
    setStatus(null);
    setNotFound(false);
    void poll();
    const handle = setInterval(() => void poll(), POLL_MS);
    return () => clearInterval(handle);
  }, [runId, poll]);

  // Hidden until the first successful fetch for THIS run; a not-found page id
  // keeps it hidden entirely (plain terminal pages render no conductor strip).
  if (notFound || !status) return null;

  const { subtasks, loop_phase, loop_running, error, run } = status;
  const counts = countByState(subtasks);
  const total = subtasks.length;
  const orderedStates: SubtaskState[] = [
    "working",
    "input_required",
    "submitted",
    "completed",
    "failed",
    "canceled",
  ];

  // Phase chip color: green when complete, red on error, blue while running,
  // muted when idle/stopped.
  const phaseColor =
    loop_phase === "complete"
      ? "#9ece6a"
      : loop_phase === "error"
        ? "#f7768e"
        : loop_running
          ? "#7aa2f7"
          : "#565f89";

  return (
    <div
      data-page-element="conductor-status-strip"
      data-run-id={run.run_id}
      data-loop-phase={loop_phase}
      className="flex items-center gap-2 px-2 py-1 bg-[#13141f]/60 backdrop-blur-sm border-b border-[#2a2d3d]/30 shrink-0 overflow-x-auto scrollbar-dark"
      role="status"
      aria-label="Conductor run status"
    >
      {/* Live loop phase */}
      <span
        className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium leading-none whitespace-nowrap"
        style={{ color: phaseColor }}
        title={`Conductor loop phase: ${loop_phase}${run.recipe ? ` · recipe: ${run.recipe}` : ""}`}
      >
        {loop_running ? (
          <Loader2 className="w-2.5 h-2.5 animate-spin" />
        ) : (
          <Activity className="w-2.5 h-2.5" />
        )}
        {phaseLabel(loop_phase)}
      </span>

      {/* Subtask count summary */}
      <span
        className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] leading-none whitespace-nowrap text-[#a9b1d6]"
        title={`${total} subtask${total === 1 ? "" : "s"} in this run`}
      >
        <GitBranch className="w-2.5 h-2.5" />
        {total} task{total === 1 ? "" : "s"}
      </span>

      {/* Per-state counts */}
      <span className="flex items-center gap-2 text-[10px] leading-none whitespace-nowrap">
        {orderedStates.map((st) =>
          counts[st] > 0 ? (
            <span key={st} className="flex items-center gap-1" style={{ color: STATE_COLORS[st] }}>
              <span
                className="w-1.5 h-1.5 rounded-full"
                style={{ backgroundColor: STATE_COLORS[st] }}
                aria-hidden
              />
              {counts[st]} {STATE_LABELS[st]}
            </span>
          ) : null,
        )}
      </span>

      {/* Compact DAG row — one chip per subtask in idx order, colored by
          state. A `↳` marker prefixes any subtask that has dependencies so
          the org-chart's layered shape is readable inline. */}
      {total > 0 && (
        <span className="flex items-center gap-1 ml-1" aria-label="Subtask DAG">
          {[...subtasks]
            .sort((a, b) => a.idx - b.idx)
            .map((st) => (
              <span
                key={st.task_id}
                className="flex items-center gap-0.5 px-1 py-0.5 rounded text-[9px] font-mono leading-none whitespace-nowrap border"
                style={{
                  color: STATE_COLORS[st.state],
                  borderColor: `${STATE_COLORS[st.state]}55`,
                  backgroundColor: `${STATE_COLORS[st.state]}11`,
                }}
                title={`${st.task_id} · ${st.title} · ${st.phase} · ${st.state}${
                  st.depends_on.length ? ` · depends on: ${st.depends_on.join(", ")}` : ""
                }`}
              >
                {st.depends_on.length > 0 && <span aria-hidden>↳</span>}
                {st.task_id}
              </span>
            ))}
        </span>
      )}

      {error && (
        <span
          className="px-1.5 py-0.5 rounded text-[10px] leading-none whitespace-nowrap text-[#f7768e]"
          title={error}
        >
          ⚠ {error.length > 40 ? `${error.slice(0, 40)}…` : error}
        </span>
      )}
    </div>
  );
}

/**
 * The hidden-worker reversal, as pure functions over plain values.
 *
 * Closing a Conductor worker's grid cell is a VIEW operation — the worker keeps
 * running — so the close has to be undoable, and the undo has to be HONEST
 * about the workers it could not bring back. That behaviour lives in three
 * places in `useTerminalManager` (`closeTerminal`, `restoreHiddenWorkers`,
 * `maybeAdoptWorker`), each of which mutates a `dismissedWorkerIds` set and a
 * `hiddenWorkers` list together and must keep them agreeing.
 *
 * It is extracted here because the runner's vitest environment is `node` with
 * no React Testing Library, so a hook's stateful behaviour is otherwise only
 * reachable by asserting on the hook file's SOURCE TEXT — which a prettier
 * reflow breaks and a logic revert does not. These functions carry the
 * reversal's actual state transitions, so the tests below them fail when the
 * reversal stops working rather than when the formatting moves.
 *
 * Every function is pure: it returns new values and mutates nothing, including
 * the `Set`s. The hook owns where the results are stored (state vs. ref) and
 * the async adoption probe; this module owns what the values become.
 */

import type { HiddenWorker } from "./useTerminalManager";

/**
 * The two stores the reversal keeps in step.
 *
 * `dismissed` is what suppresses the live adoption probe (without it a closed
 * cell reappears on the worker's next `ai-output` line, which is a
 * three-second hide rather than a close). `hidden` is the same dismissals as
 * renderable rows for the "N hidden workers" chip. A dismissal with no row is
 * a worker the operator can never get back; a row with no dismissal is a chip
 * entry for a cell that is about to reappear on its own. Both are bugs, which
 * is why the two move together here.
 */
export interface HiddenWorkerState {
  hidden: readonly HiddenWorker[];
  dismissed: ReadonlySet<string>;
}

export const EMPTY_HIDDEN_WORKER_STATE: HiddenWorkerState = {
  hidden: [],
  dismissed: new Set(),
};

/** Every id a dismissal is recorded under: the tab id and, when present, the task run id. */
export function workerDismissalKeys(worker: {
  tabId: string;
  taskRunId: string | null;
}): string[] {
  return worker.taskRunId && worker.taskRunId !== worker.tabId
    ? [worker.tabId, worker.taskRunId]
    : [worker.tabId];
}

/**
 * Record a closed worker view. Idempotent on `tabId`: closing a tab that is
 * already listed does not duplicate the row (React may invoke an updater
 * twice in StrictMode).
 */
export function hideWorker(state: HiddenWorkerState, worker: HiddenWorker): HiddenWorkerState {
  const dismissed = new Set(state.dismissed);
  for (const key of workerDismissalKeys(worker)) dismissed.add(key);
  const hidden = state.hidden.some((w) => w.tabId === worker.tabId)
    ? state.hidden
    : [...state.hidden, worker];
  return { hidden, dismissed };
}

/**
 * Begin a restore: pick the entries to bring back, clear their dismissals, and
 * drop them from the chip.
 *
 * The dismissal is cleared BEFORE the adoption probe runs and stays cleared
 * even if that probe misses — a miss is often transient (a failed
 * `terminal_session_list_open`, or a record not yet written), and with the
 * dismissal gone the live probe adopts the worker on its next event.
 *
 * `probeKeys` are the throttle entries the caller must drop so the adoption
 * read happens NOW rather than on the worker's next event; a quiet worker
 * would otherwise stay invisible after an explicit "show", which is the
 * silent-no-op this whole affordance exists to remove.
 */
export function beginRestore(
  state: HiddenWorkerState,
  tabIds?: readonly string[],
): { state: HiddenWorkerState; restoring: readonly HiddenWorker[]; probeKeys: readonly string[] } {
  const restoring = state.hidden.filter((w) => !tabIds || tabIds.includes(w.tabId));
  if (restoring.length === 0) {
    return { state, restoring: [], probeKeys: [] };
  }
  const dismissed = new Set(state.dismissed);
  for (const w of restoring) {
    for (const key of workerDismissalKeys(w)) dismissed.delete(key);
  }
  const restoringIds = new Set(restoring.map((w) => w.tabId));
  return {
    state: { hidden: state.hidden.filter((w) => !restoringIds.has(w.tabId)), dismissed },
    restoring,
    probeKeys: restoring.map((w) => w.taskRunId ?? w.tabId),
  };
}

/**
 * Re-list the workers a restore could NOT adopt, stamped `restoreMissedAtMs`.
 * The dismissal stays cleared (see `beginRestore`) — this is a report, not a
 * re-close. A click that silently does nothing is the failure being fixed, so
 * a miss is always visible in the chip's tooltip.
 */
export function recordRestoreMisses(
  state: HiddenWorkerState,
  missed: readonly HiddenWorker[],
  atMs: number,
): HiddenWorkerState {
  if (missed.length === 0) return state;
  const additions = missed
    .filter((m) => !state.hidden.some((w) => w.tabId === m.tabId))
    .map((m) => ({ ...m, restoreMissedAtMs: atMs }));
  if (additions.length === 0) return state;
  return { ...state, hidden: [...state.hidden, ...additions] };
}

/**
 * The worker is on screen again, so it is no longer hidden — drop any row for
 * it, including a "could not be re-opened" one left by a failed restore.
 * Returns the same state object when there was nothing to drop, so a caller
 * storing this in React state can skip the re-render.
 */
export function forgetAdoptedWorker(
  state: HiddenWorkerState,
  tabId: string,
  taskRunId: string,
): HiddenWorkerState {
  if (!state.hidden.some((w) => w.tabId === tabId || w.taskRunId === taskRunId)) return state;
  return {
    ...state,
    hidden: state.hidden.filter((w) => w.tabId !== tabId && w.taskRunId !== taskRunId),
  };
}

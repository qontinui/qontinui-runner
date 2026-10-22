/**
 * The deferred-kill close, as pure functions over plain values.
 *
 * Closing a zone used to kill its PTY on the spot: no confirmation, no way
 * back, and a Claude session mid-turn lost the turn it was in. A single-target
 * user close now removes the tab from its zone AT ONCE but holds the kill
 * (`terminal_close` + `terminal_session_record_close`) back for
 * {@link PENDING_CLOSE_WINDOW_MS}. Undo inside that window puts the tab back
 * with its PTY never having been touched — the in-flight turn survives because
 * nothing interrupted it, not because anything resumed it. When the window
 * expires the kill fires exactly once.
 *
 * Bulk and programmatic closes (prune, reattach, restart-in-zone, a failed AI
 * launch's orphan, the project move) opt out with `immediate: true`: they
 * replace or sweep tabs on the operator's behalf, and a ten-second ghost PTY
 * behind each of them would be a leak with nothing to undo.
 *
 * Same shape as `hiddenWorkerReducer.ts`, and for the same reason: the runner's
 * vitest environment is `node` with no React Testing Library, so the hook's
 * state transitions are only testable once they are pure. `useTerminalManager`
 * owns the timers, the toast and the Tauri calls; this module owns what the
 * pending set becomes and what each close is routed to.
 */

import { ACTION_DISMISS_MS } from "@/hooks/useToast";
import type { TerminalTab } from "./useTerminalManager";

/**
 * How long a closed tab can be brought back. Exactly the lifetime of the
 * toast that carries the Undo button: a window longer than the button is a
 * PTY kept alive for nobody, and a shorter one is a button that lies.
 */
export const PENDING_CLOSE_WINDOW_MS = ACTION_DISMISS_MS;

/** A closed tab whose PTY has not been killed yet. */
export interface PendingClose {
  /** The tab exactly as it was when closed — what Undo puts back. */
  tab: TerminalTab;
  /** Its position in the tab list, so Undo restores the same order. */
  index: number;
  /** Whether it was the active tab, so Undo can re-select it. */
  wasActive: boolean;
  closedAtMs: number;
  /**
   * Put the tab back into the zone it occupied. Supplied by the session
   * provider, which owns the zone layout; absent when the tab had no zone.
   */
  restoreZone?: () => void;
}

export interface PendingCloseState {
  pending: ReadonlyMap<string, PendingClose>;
}

export const EMPTY_PENDING_CLOSE_STATE: PendingCloseState = { pending: new Map() };

/**
 * What a `closeTerminal(id, { immediate })` call does.
 *
 *  - `already-pending`  a second deferred close of a tab already counting
 *                       down: nothing (it keeps its one timer and one toast).
 *  - `expedite-pending` an immediate close of a pending tab: kill it now,
 *                       once, and cancel its countdown.
 *  - `hide-worker`      a Conductor worker view: hide it (the reversible close
 *                       `hiddenWorkerReducer` already owns); no PTY to kill.
 *  - `remove-view`      a plan tab: drop it; it has no PTY.
 *  - `kill-now`         an opted-out close, or an id with no tab to snapshot
 *                       (there would be nothing for Undo to put back).
 *  - `defer`            a single-target user close: remove now, kill later.
 */
export type CloseRoute =
  | "already-pending"
  | "expedite-pending"
  | "hide-worker"
  | "remove-view"
  | "kill-now"
  | "defer";

export function routeClose(
  state: PendingCloseState,
  id: string,
  tab: TerminalTab | undefined,
  immediate: boolean,
): CloseRoute {
  if (state.pending.has(id)) return immediate ? "expedite-pending" : "already-pending";
  if (tab?.sessionBacked === true) return "hide-worker";
  if (id.startsWith("plan-") || tab?.type === "plan") return "remove-view";
  if (immediate || !tab) return "kill-now";
  return "defer";
}

/** Start a close's countdown. Idempotent on the tab id. */
export function beginPendingClose(
  state: PendingCloseState,
  entry: PendingClose,
): PendingCloseState {
  if (state.pending.has(entry.tab.id)) return state;
  const pending = new Map(state.pending);
  pending.set(entry.tab.id, entry);
  return { pending };
}

/**
 * Take one entry out of the pending set. Shared by undo and expiry: whichever
 * runs first gets the entry, and the other finds nothing — which is what makes
 * "undo after expiry" a no-op and "expire after undo" never kill.
 */
function take(
  state: PendingCloseState,
  id: string,
): { state: PendingCloseState; entry: PendingClose | null } {
  const entry = state.pending.get(id);
  if (!entry) return { state, entry: null };
  const pending = new Map(state.pending);
  pending.delete(id);
  return { state: { pending }, entry };
}

/** Undo: the entry to restore, or `null` when it already expired (no-op). */
export function undoPendingClose(
  state: PendingCloseState,
  id: string,
): { state: PendingCloseState; restored: PendingClose | null } {
  const { state: next, entry } = take(state, id);
  return { state: next, restored: entry };
}

/** Expiry: the entry to kill, or `null` when it was undone or already killed. */
export function expirePendingClose(
  state: PendingCloseState,
  id: string,
): { state: PendingCloseState; kill: PendingClose | null } {
  const { state: next, entry } = take(state, id);
  return { state: next, kill: entry };
}

/**
 * Unmount / page hide: every pending close becomes a kill now. The operator
 * closed these tabs; leaving their PTYs alive because the page went away first
 * would leak them.
 */
export function flushPendingCloses(state: PendingCloseState): {
  state: PendingCloseState;
  kills: PendingClose[];
} {
  if (state.pending.size === 0) return { state, kills: [] };
  return { state: EMPTY_PENDING_CLOSE_STATE, kills: [...state.pending.values()] };
}

/**
 * The backend terminal list with pending closes removed, for the re-sync.
 *
 * A pending tab's PTY is still alive, so `terminal_list` still lists it, and
 * the re-sync adds every listed terminal that has no tab — without this filter
 * a closed tab would reappear on the next exit or close elsewhere on the page.
 * Returns the SAME array when nothing is filtered.
 */
export function excludePendingCloses<T extends { id: string }>(
  terminals: readonly T[],
  state: PendingCloseState,
): readonly T[] {
  if (state.pending.size === 0) return terminals;
  const kept = terminals.filter((t) => !state.pending.has(t.id));
  return kept.length === terminals.length ? terminals : kept;
}

/**
 * Put an undone tab back at the position it had. Returns the SAME array when
 * a tab with that id is already present.
 */
export function restorePendingTab(tabs: TerminalTab[], entry: PendingClose): TerminalTab[] {
  if (tabs.some((t) => t.id === entry.tab.id)) return tabs;
  const at = Math.min(Math.max(0, entry.index), tabs.length);
  return [...tabs.slice(0, at), entry.tab, ...tabs.slice(at)];
}

/** The toast text for a close: the name the operator knows the session by. */
export function pendingCloseMessage(tab: TerminalTab): string {
  return `Closed ${tab.spawnName?.trim() || tab.title}`;
}

/**
 * "Keep AI sessions" — the pure core behind `PruneTerminalsButton`.
 *
 * One click closes every window on the page that is NOT hosting an AI session
 * (plain shells, plan viewers) and compacts the grid down to the smallest
 * uniform layout that fits the survivors, via the same {@link pickLayout}
 * ladder every other grow path uses. A 9-zone grid holding 3 AI sessions,
 * 3 plain shells and 3 empty zones becomes a 4-zone quad: the 3 sessions in
 * zones 0-2 and one empty zone.
 *
 * Pure so the decision table is unit-testable in the node vitest env without
 * React; the button does the closing and `useZoneLayout.requestCompaction`
 * applies the layout once the closes have landed.
 */

import type { TerminalTab } from "./useTerminalManager";
import { pickLayout, type SessionState, type ZoneAssignments } from "./useZoneLayout";

type PruneTab = Pick<
  TerminalTab,
  | "id"
  | "title"
  | "createdAt"
  | "claudeSessionId"
  | "sessionBacked"
  | "taskRunId"
  | "remote"
  | "__synthetic"
>;

/**
 * A tab younger than this is kept even without a session id. Every launch path
 * stamps `claudeSessionId` AFTER the PTY exists: the launcher pin after an
 * awaited command build, the custom-command capture poll after up to ~15s, the
 * reconcile binder's `session-bound` a poll tick after the provider starts.
 * A prune inside that window must not kill the session it has not heard of yet.
 */
export const RECENT_TAB_GRACE_MS = 60_000;

/** Live signals beyond the tab's own fields; both optional. */
export interface PruneSignals {
  /** `useSessionStateTracking`'s per-tab state. */
  sessionStates?: Readonly<Record<string, SessionState>>;
  /** Epoch ms "now", for the {@link RECENT_TAB_GRACE_MS} window. */
  now?: number;
}

/**
 * Does this tab host an AI session?
 *
 * - `claudeSessionId` — stamped on every provider launch (the spawn-time
 *   `--session-id` pin), every resume, and every `session-bound` event the
 *   reconcile binder emits for a hand-typed launch. A plain shell never
 *   carries one.
 * - `sessionBacked` / `taskRunId` — a Conductor worker view, which IS an AI
 *   session even though no PTY backs it.
 * - `remote` — a mirror of a session on another device. Closing it detaches
 *   the relay binding, and the session behind it is not this page's to judge,
 *   so it is kept.
 *
 * Those fields can lag the session itself (see {@link RECENT_TAB_GRACE_MS}),
 * and a custom launch command whose capture poll never binds leaves none of
 * them set at all. So two live signals also keep a tab: it was created inside
 * the grace window, or the session-state detector reads it as `working` or
 * `needs-input` (a Claude UI mid-turn or awaiting approval; a busy plain shell
 * is kept too, which is the safe direction).
 *
 * Errs toward KEEPING: a tab is closed only when no signal says otherwise.
 */
export function hasAiSession(tab: PruneTab, signals: PruneSignals = {}): boolean {
  if (tab.claudeSessionId || tab.sessionBacked === true || tab.taskRunId || tab.remote != null) {
    return true;
  }
  const state = signals.sessionStates?.[tab.id];
  if (state === "working" || state === "needs-input") return true;
  const { now } = signals;
  return (
    now !== undefined && tab.createdAt !== undefined && now - tab.createdAt < RECENT_TAB_GRACE_MS
  );
}

export interface PrunePlan {
  /** Tabs to close, in tab order. */
  closeIds: string[];
  /** Titles of `closeIds`, same order: what the confirm names. */
  closeTitles: string[];
  /** Tabs to keep, in their target zone order (zone 0 first). */
  keepIds: string[];
  /** Layout the kept tabs compact into. */
  layoutId: string;
}

/**
 * Split the page's tabs into close / keep and pick the compacted layout.
 *
 * Kept tabs keep their RELATIVE grid order: assigned tabs by ascending zone
 * index, then unassigned (hidden) AI tabs in tab order — so compaction pulls a
 * hidden session onto the grid rather than leaving it off-screen. Synthetic
 * fixture tabs are never rendered and are ignored entirely.
 */
export function planPrune(
  tabs: readonly PruneTab[],
  assignments: ZoneAssignments,
  signals: PruneSignals = {},
): PrunePlan {
  const zoneOf = new Map<string, number>();
  for (const [z, id] of Object.entries(assignments)) {
    if (id) zoneOf.set(id, Number(z));
  }

  const closeIds: string[] = [];
  const closeTitles: string[] = [];
  const keep: { id: string; order: number; zone: number }[] = [];
  tabs.forEach((tab, order) => {
    if (tab.__synthetic) return;
    if (hasAiSession(tab, signals)) {
      keep.push({ id: tab.id, order, zone: zoneOf.get(tab.id) ?? Number.POSITIVE_INFINITY });
    } else {
      closeIds.push(tab.id);
      closeTitles.push(tab.title);
    }
  });
  keep.sort((a, b) => a.zone - b.zone || a.order - b.order);
  const keepIds = keep.map((k) => k.id);

  return { closeIds, closeTitles, keepIds, layoutId: pickLayout(keepIds.length) };
}

/**
 * Would pruning change anything? True when there is a window to close, or the
 * kept tabs are not already a dense prefix of a layout no larger than the one
 * `pickLayout` would choose. Gates the button so it never offers a no-op.
 */
export function isPruneActionable(
  plan: PrunePlan,
  currentZoneCount: number,
  targetZoneCount: number,
  assignments: ZoneAssignments,
): boolean {
  if (plan.closeIds.length > 0) return true;
  if (currentZoneCount > targetZoneCount) return true;
  return plan.keepIds.some((id, i) => assignments[i] !== id);
}

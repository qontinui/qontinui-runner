/**
 * The triage chords' targets (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5) — what `useKeyboardShortcuts` acts on:
 *
 * - **Ctrl+Shift+N, "next session that needs you".** A tab waiting for input
 *   in this window is still the first target (the zone / tmux walk). When no
 *   tab needs input, it falls through to the next Session Manager row reading
 *   `Needs you` — a session needing the operator that no tab here shows (a
 *   registry `waiting` row, one outside this runner). Only when there is none
 *   does the walk's errored-tab fallback run.
 * - **Ctrl+Shift+↓, "next in group".** From the session on screen, the next
 *   member of its group in the group's own order, wrapping.
 *
 * Pure: no React, no DOM.
 */

import { nextWithActivity, type SessionActivity } from "./sessionStatusModel";

export type NeedsYouJump<R> =
  /** Walk the tabs (needs-input first, then the errored fallback). */
  | { kind: "tabs" }
  /** No tab needs input: open this row. */
  | { kind: "row"; row: R };

/** Where Ctrl+Shift+N goes. `tabStates` are this window's live tabs' states. */
export function nextNeedsYouJump<
  R extends { sessionId: string; status: { activity: SessionActivity } },
>(
  tabStates: readonly (string | undefined)[],
  rows: readonly R[],
  afterRowId: string | null,
): NeedsYouJump<R> {
  if (tabStates.some((s) => s === "needs-input")) return { kind: "tabs" };
  const row = nextWithActivity(rows, "needs-you", afterRowId);
  return row ? { kind: "row", row } : { kind: "tabs" };
}

/** A row's place in its group: `null` = in no group (or the org is not known). */
export interface GroupPlace {
  groupId: string;
  order: number;
}

/**
 * The member after `currentRowId` in its group, by the group's order,
 * wrapping; `null` when the current row is in no group or is its only
 * listed member.
 */
export function nextInGroup<R extends { sessionId: string }>(
  rows: readonly R[],
  currentRowId: string | null,
  placeOf: (row: R) => GroupPlace | null,
): R | null {
  if (currentRowId === null) return null;
  const current = rows.find((r) => r.sessionId === currentRowId);
  const place = current ? placeOf(current) : null;
  if (!current || !place) return null;
  const members = rows
    .map((row) => ({ row, place: placeOf(row) }))
    .filter((m) => m.place?.groupId === place.groupId)
    .sort((a, b) => (a.place?.order ?? 0) - (b.place?.order ?? 0))
    .map((m) => m.row);
  if (members.length < 2) return null;
  const at = members.indexOf(current);
  return members[(at + 1) % members.length];
}

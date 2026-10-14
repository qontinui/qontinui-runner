/**
 * Reopening a recently closed session — Chrome's "reopen closed tab" for
 * sessions (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5).
 *
 * A reopen RESUMES the session's transcript in a new tab and puts it back in
 * the group it was closed from (the store recorded that group with the close).
 * It never resumes a session that may still be running — a second
 * `claude --resume` of a live conversation forks it — so a row that is live
 * here is focused instead, and one running elsewhere, or whose placement is
 * unknown, is refused with the reason.
 *
 * Pure: no React, no Tauri.
 */

import type { ClosedSession, SessionOrg, SessionOrgOp } from "./sessionOrgTypes";
import type { SessionLocation } from "./sessionStatusModel";

/** The row fields a reopen reads. */
export interface ReopenRow {
  sessionId: string;
  source: "transcript" | "tab";
  status: { location: SessionLocation };
}

export type ReopenPlan<R extends ReopenRow> =
  /** Resume `row`, then apply `ops` (the group restore). */
  | { kind: "resume"; row: R; ops: SessionOrgOp[]; note: string | null }
  /** Already live in this window: focus it, do not fork it. */
  | { kind: "focus"; row: R }
  | { kind: "refused"; reason: string };

const LIVE_HERE: ReadonlySet<SessionLocation> = new Set<SessionLocation>([
  "on-screen",
  "hidden-here",
]);

/** The ops that put a reopened session back in its group, and what to say when they cannot. */
export function groupRestore(
  entry: Pick<ClosedSession, "sessionKey" | "groupId">,
  org: SessionOrg | null,
): { ops: SessionOrgOp[]; note: string | null } {
  if (entry.groupId === null) return { ops: [], note: null };
  if (org === null) {
    return { ops: [], note: "its group could not be restored: the organization store is unknown" };
  }
  if (!org.groups.some((g) => g.id === entry.groupId)) {
    return { ops: [], note: "its group no longer exists; it reopened ungrouped" };
  }
  if (org.members[entry.sessionKey]?.groupId === entry.groupId) return { ops: [], note: null };
  return {
    ops: [{ op: "addMembers", groupId: entry.groupId, sessionKeys: [entry.sessionKey] }],
    note: null,
  };
}

/** What reopening `entry` would do, given the rows the manager lists now. */
export function planReopen<R extends ReopenRow>(
  entry: ClosedSession,
  rows: readonly R[],
  org: SessionOrg | null,
): ReopenPlan<R> {
  if (entry.resumeCmd === null) {
    return { kind: "refused", reason: "it closed with no resumable transcript" };
  }
  const row = rows.find((r) => r.source === "transcript" && r.sessionId === entry.sessionKey);
  if (!row) return { kind: "refused", reason: "its transcript is not listed on this runner" };
  const location = row.status.location;
  if (LIVE_HERE.has(location)) return { kind: "focus", row };
  if (location === "unknown") {
    return {
      kind: "refused",
      reason: "whether it is already running is unknown — a resume could fork it",
    };
  }
  if (location !== "not-running") {
    return { kind: "refused", reason: "it is running outside this runner — a resume would fork it" };
  }
  return { kind: "resume", row, ...groupRestore(entry, org) };
}

/** The newest entry a reopen chord would act on: the first that is not refused. */
export function lastReopenable<R extends ReopenRow>(
  entries: readonly ClosedSession[],
  rows: readonly R[],
  org: SessionOrg | null,
): { entry: ClosedSession; plan: ReopenPlan<R> } | null {
  for (const entry of entries) {
    const plan = planReopen(entry, rows, org);
    if (plan.kind === "resume") return { entry, plan };
  }
  return null;
}

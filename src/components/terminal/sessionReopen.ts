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

import { describeThrown } from "@/lib/utils";

import type {
  ClosedSession,
  SessionOrg,
  SessionOrgApplyResponse,
  SessionOrgOp,
} from "./sessionOrgTypes";
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
  const now = org.members[entry.sessionKey]?.groupId ?? null;
  if (now === entry.groupId) return { ops: [], note: null };
  if (now !== null) {
    // Moved to another group since it closed: that later choice stands.
    const title = org.groups.find((g) => g.id === now)?.title ?? now;
    return { ops: [], note: `it was moved to ${title} after it closed; it stays there` };
  }
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
    return {
      kind: "refused",
      reason: "it is running outside this runner — a resume would fork it",
    };
  }
  return { kind: "resume", row, ...groupRestore(entry, org) };
}

/**
 * The newest entry the reopen chord acts on: the first that would RESUME.
 * An entry already open here (a focus) or refused is passed over, so pressing
 * the chord again walks back through the list as Chrome's Ctrl+Shift+T does.
 */
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

/** One reopen's outcome. */
export interface ReopenResult {
  state: "done" | "not-done" | "unknown";
  detail: string;
}

export interface ReopenEffects<R> {
  resume: (row: R) => void;
  focus: (row: R) => void;
  applyOrg: (ops: SessionOrgOp[]) => Promise<SessionOrgApplyResponse>;
}

/**
 * Reopen `entry`: resume (or focus) it, then put it back in the group it
 * closed from. The restore is READ BACK from the snapshot the apply answered —
 * an op that said ok while the store shows the session elsewhere is "not
 * done", and a store that could not be read back is "unknown", never "done".
 */
export async function reopenClosedSession<R extends ReopenRow>(
  entry: ClosedSession,
  rows: readonly R[],
  org: SessionOrg | null,
  effects: ReopenEffects<R>,
): Promise<ReopenResult> {
  const plan = planReopen(entry, rows, org);
  if (plan.kind === "refused") return { state: "not-done", detail: `not reopened: ${plan.reason}` };
  try {
    if (plan.kind === "focus") {
      effects.focus(plan.row);
      return { state: "done", detail: "Already open — focused it" };
    }
    effects.resume(plan.row);
  } catch (e) {
    return { state: "not-done", detail: `not reopened: ${describeThrown(e, "resume failed")}` };
  }
  if (plan.ops.length === 0) {
    return { state: "done", detail: `Reopened${plan.note ? ` — ${plan.note}` : ""}` };
  }
  let response: SessionOrgApplyResponse;
  try {
    response = await effects.applyOrg(plan.ops);
  } catch (e) {
    return {
      state: "unknown",
      detail: `Reopened; whether it went back in its group is unknown: ${describeThrown(e, "session_org_apply failed")}`,
    };
  }
  const after = response.snapshot.status === "known" ? response.snapshot.org : null;
  if (!after) {
    return {
      state: "unknown",
      detail: `Reopened; whether it went back in its group is unknown: ${response.snapshot.reason ?? "the store could not be read back"}`,
    };
  }
  if (after.members[entry.sessionKey]?.groupId === entry.groupId) {
    return { state: "done", detail: "Reopened in its group" };
  }
  const refusals = response.results
    .map((r) => r.error)
    .filter((e): e is string => Boolean(e))
    .join(", ");
  return {
    state: "not-done",
    detail: `Reopened, but not put back in its group${refusals ? ` (refused: ${refusals})` : ""}`,
  };
}

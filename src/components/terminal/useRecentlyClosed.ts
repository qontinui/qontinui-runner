/**
 * `useRecentlyClosed` — the Session Manager's Recently closed list and its
 * reopen (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5). The list is the organization store's `recentlyClosed` (every close
 * records into it, `sessionClose.ts`); a reopen is planned by
 * `sessionReopen.ts` and answers what it did, including whether the session
 * went back into its group.
 */

import { useCallback, useMemo } from "react";

import {
  lastReopenable,
  reopenClosedSession,
  type ReopenResult,
  type ReopenRow,
} from "./sessionReopen";
import type { ClosedSession, SessionOrgApplyResponse, SessionOrgOp } from "./sessionOrgTypes";
import type { SessionOrgView } from "./useSessionOrg";

export type { ReopenResult } from "./sessionReopen";

/** The list as a surface renders it; `null` entries = the store is not known. */
export interface RecentlyClosedView {
  entries: readonly ClosedSession[] | null;
  /** Every close ever recorded; `entries` holds the newest of them. */
  total: number | null;
  /** Why `entries` is null. */
  unknownReason: string | null;
}

export interface UseRecentlyClosedDeps<R extends ReopenRow> {
  org: SessionOrgView;
  rows: readonly R[];
  applyOrg: (ops: SessionOrgOp[]) => Promise<SessionOrgApplyResponse>;
  resume: (row: R) => void;
  focus: (row: R) => void;
}

export interface UseRecentlyClosedResult {
  recentlyClosed: RecentlyClosedView;
  reopenClosed: (entry: ClosedSession) => Promise<ReopenResult>;
  /** The chord: reopen the newest entry that can be reopened; `null` = none can. */
  reopenLastClosed: () => Promise<ReopenResult | null>;
}

export function useRecentlyClosed<R extends ReopenRow>({
  org,
  rows,
  applyOrg,
  resume,
  focus,
}: UseRecentlyClosedDeps<R>): UseRecentlyClosedResult {
  const known = org.phase === "known" ? org.org : null;

  const recentlyClosed = useMemo<RecentlyClosedView>(
    () =>
      known
        ? { entries: known.recentlyClosed, total: known.recentlyClosedTotal, unknownReason: null }
        : {
            entries: null,
            total: null,
            unknownReason:
              org.phase === "unknown" ? org.reason : "the organization store is still loading",
          },
    [known, org],
  );

  const run = useCallback(
    (entry: ClosedSession): Promise<ReopenResult> =>
      reopenClosedSession(entry, rows, known, { resume, focus, applyOrg }),
    [rows, known, applyOrg, resume, focus],
  );

  const reopenLastClosed = useCallback(async () => {
    if (!known) return null;
    const next = lastReopenable(known.recentlyClosed, rows, known);
    return next ? run(next.entry) : null;
  }, [known, rows, run]);

  return { recentlyClosed, reopenClosed: run, reopenLastClosed };
}

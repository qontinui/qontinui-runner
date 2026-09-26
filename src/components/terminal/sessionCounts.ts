/**
 * Session / tab count reconciliations for the Terminal page.
 *
 * The Terminal page counts three genuinely different populations that
 * routinely and legitimately differ in both directions:
 *
 *   - **Claude sessions** (`useSessionManager`'s `claudeSessionCount`) —
 *     machine-wide transcript sessions, including `active-external` ones with
 *     no tab in this window.
 *   - **Open panes** (`tabs.length`) — PTY tabs owned by this page.
 *   - **Zones** (`layout.zones.length`) — CSS grid cells.
 *
 * The helpers below are the reconciliation rules between those populations,
 * promoted out of `StatusStrip.tsx` so every surface (the strip, the window
 * title, …) imports ONE rule instead of re-deriving it. The governing
 * invariant: the count a control claims must be the count that control can
 * reach.
 */

import type { SessionState } from "./useZoneLayout";

/**
 * How many PTY TABS on this page are currently in `state`.
 *
 * This is the evidence every *actionable* affordance on the terminal page
 * already runs on: the zone renderer's colouring, `focusNextNeedsInput` /
 * `focusNextError`'s walk over `zoneLayout.assignments`, and `BatchActions`'
 * `needsInputTabs`. Keyed by tab id, filtered through the live `tabs` list so
 * a stale `sessionStates` entry for a closed tab can't inflate it.
 */
export function countTabsInState(
  tabs: readonly { id: string }[] | undefined | null,
  sessionStates: Record<string, SessionState> | undefined | null,
  state: SessionState,
): number {
  if (!tabs || !sessionStates) return 0;
  let n = 0;
  for (const tab of tabs) {
    if (sessionStates[tab.id] === state) n++;
  }
  return n;
}

/**
 * The error count the strip actually shows: the Claude-session bucketing
 * UNIONed with the page's own tab-scoped `error` states.
 *
 * THE DEFECT: `statusCounts.errorCount` buckets *Claude sessions*
 * (`useSessionManager`), while everything else on this page — the zone
 * renderer's red border, and `focusNextError`'s walk right below — reads
 * `sessionStates[tabId]`, keyed by PTY tab. A tab whose PTY died carries
 * `sessionStates[tab] === "error"` but has no live Claude session to bucket,
 * so the pill read `0 errors` (and `hasContent` hid the strip outright) on a
 * page that was simultaneously painting that tab as errored and would happily
 * cycle to it. The count and the cycler have to answer to the same evidence.
 *
 * `Math.max` is the union, not a fudge: the two sets OVERLAP (a tab-backed
 * session in error is counted by both) and share no key to dedupe on —
 * `sessionStates` is keyed by terminal-tab id, `statusCounts` is bucketed over
 * session records. Max can never double-count an overlapping error and never
 * reads below either input, so the pill is present whenever either surface
 * has something to point at.
 */
export function unionErrorCount(sessionErrorCount: number, tabErrorCount: number): number {
  return Math.max(sessionErrorCount, tabErrorCount);
}

/**
 * How many PTY TABS on this page are currently LIVE.
 *
 * Counts `isAlive` truthily — the same spelling of liveness the rest of this
 * page already runs on (`TerminalSessionContext`'s `liveTabIds`,
 * `useProjectTerminalReconcile`'s `if (!t.isAlive) return false`, and
 * `buildTerminalSessionRoster`'s `isAlive: Boolean(t.isAlive)`). A tab whose
 * PTY has exited is a tombstone the operator cannot work in, so counting every
 * historical tab would re-open the strip on a page with nothing running — the
 * mirror image of the defect below, and exactly the inflation
 * `statusCounts` was introduced to avoid (18 tabs -> "18 sessions").
 */
export function countLiveTabs(tabs: readonly { isAlive?: boolean }[] | undefined | null): number {
  if (!tabs) return 0;
  let n = 0;
  for (const tab of tabs) {
    if (tab.isAlive) n++;
  }
  return n;
}

/**
 * The session count the strip's multi-zone pills gate on: the Claude-session
 * bucketing UNIONed with this page's own live PTY tabs.
 *
 * THE DEFECT: `hasContent` read a UNIONed `errorCount` (see
 * {@link unionErrorCount}) right beside an un-unioned `isMultiZone`, which was
 * a bare `sessionCount > 1` off `useSessionManager.statusCounts`. That count
 * buckets *Claude sessions* — the comment at its destructuring site says so
 * outright — so two live PTY tabs with no Claude session attached scored 0 and
 * the whole status surface refused to render on a page that plainly had two
 * terminals in it. One boolean expression cannot honestly mix a unioned input
 * with an un-unioned one.
 *
 * `Math.max` is the union for the same reason it is in {@link unionErrorCount}:
 * the two sets OVERLAP (a tab-backed Claude session is counted by both) and
 * share no key to dedupe on — `tabs` is keyed by terminal-tab id,
 * `statusCounts` is bucketed over session records. Max can never double-count
 * an overlapping session and never reads below either input.
 */
export function unionSessionCount(claudeSessionCount: number, liveTabCount: number): number {
  return Math.max(claudeSessionCount, liveTabCount);
}

/**
 * Split the needs-input signal into what this page can ACT on and what it can
 * only report.
 *
 * THE DEFECT: the pill counted Claude sessions while the two things it
 * advertises — "Tab to cycle" (`focusNextNeedsInput`, a walk over zone
 * assignments) and the `BatchActions` buttons rendered beside it
 * (`needsInputTabs`) — both operate on PTY tabs. An active-EXTERNAL session
 * waiting for input has no tab in this window, so the strip claimed "2 need
 * input · Tab to cycle" while cycling reached one of them and Approve-all
 * would have written to one. The count a control claims must be the count
 * that control can reach.
 *
 * So the headline number is the tab-scoped one, and the remainder is
 * surfaced separately as `+N external` — reported, not silently folded in
 * and not silently dropped.
 */
export function splitNeedsInput(
  claudeNeedsInputCount: number,
  tabNeedsInputCount: number,
): { actionable: number; external: number } {
  return {
    actionable: tabNeedsInputCount,
    external: Math.max(0, claudeNeedsInputCount - tabNeedsInputCount),
  };
}

/**
 * Split the working count into what runs ON THIS PAGE and what runs elsewhere.
 *
 * THE DEFECT (UI-4): `workingCount` buckets `active-in-zone` AND
 * `active-external` sessions — the latter are other `claude` processes on the
 * box with no tab in this window — while the strip it headlines is labelled
 * "Terminal page status". One page worker and four external sessions read
 * "5 working", a count of work this page is not doing.
 *
 * Same shape as {@link splitNeedsInput}: the headline is page-scoped and the
 * external remainder is reported as `+M external` — surfaced, not folded in
 * and not dropped. The external share is clamped to `[0, workingCount]` so a
 * transiently inconsistent pair can never render a negative page count.
 */
export function splitWorking(
  workingCount: number,
  externalWorkingCount: number,
): { page: number; external: number } {
  const external = Math.max(0, Math.min(externalWorkingCount, workingCount));
  return { page: workingCount - external, external };
}

/**
 * Render the working segment of the state-breakdown pill: `N working`, plus
 * ` +M external` when external sessions are running. Returns `null` when there
 * is nothing to show, so the caller can gate the segment on it.
 */
export function formatWorking(split: { page: number; external: number }): string | null {
  if (split.page === 0 && split.external === 0) return null;
  const headline = `${split.page} working`;
  return split.external > 0 ? `${headline} +${split.external} external` : headline;
}

/**
 * The needs-input / error counts the WINDOW TITLE shows.
 *
 * THE DEFECT: the title counted `Object.values(sessionStates)` directly, with
 * no intersection against the live `tabs` list. A closed tab whose
 * `sessionStates` entry had not been reaped kept inflating the title's
 * waiting / error count after the tab was gone — and the title is the one
 * surface visible when the window is NOT focused, i.e. exactly when the
 * operator relies on it and cannot cross-check it against the status strip.
 *
 * So the title counts through {@link countTabsInState}, the same tab-scoped
 * evidence the strip's actionable counts and the zone cyclers run on.
 */
export function windowTitleCounts(
  tabs: readonly { id: string }[] | undefined | null,
  sessionStates: Record<string, SessionState> | undefined | null,
): { needsInputCount: number; errorCount: number } {
  return {
    needsInputCount: countTabsInState(tabs, sessionStates, "needs-input"),
    errorCount: countTabsInState(tabs, sessionStates, "error"),
  };
}

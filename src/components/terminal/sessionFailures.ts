/**
 * The frontend half of the session-failure taxonomy (plan
 * `2026-09-20-ai-session-handling-is-claude-shaped-provider-manifest-and-failure-taxonomy`,
 * Phase 7).
 *
 * The runner classifies every failure of an AI session — a scraped usage-limit
 * phrase, a stream-json child's stderr, a pane that exited on its own, a resume
 * whose handshake never appeared — into ONE `SessionFailure` shape, records the
 * active ones per terminal, and announces every change on the
 * `session-failure` event. This module holds the pure pieces the banner and
 * its hook need: the served shape, the notice reducer, and the resume
 * verifier's reporting rule. The webview holds no failure vocabulary of its
 * own; titles, details and actions come from the payload.
 */

import type { TerminalTab } from "./useTerminalManager";

/** The runner's event name for failure notices. */
export const SESSION_FAILURE_EVENT = "session-failure";

/** Actions a surface may offer for a failure (`FailureAction`). */
export type FailureAction = "retry" | "login" | "switch_account" | "new_session" | "resume" | "none";

/**
 * The fields of the served `SessionFailure` (`qontinui-schemas`
 * `rust/src/cli_session.rs`) the banner reads. `@qontinui/shared-types` 0.6.0,
 * the version the runner installs from npm, predates the type, so it is
 * declared locally — the same arrangement as `ServedCliProfile`.
 */
export interface ServedSessionFailure {
  id: string;
  kind:
    | "rate_limited"
    | "quota_exhausted"
    | "budget_exhausted"
    | "context_exhausted"
    | "auth_required"
    | "access_denied"
    | "overloaded"
    | "transport_lost"
    | "resume_failed"
    | "spawn_failed"
    | "process_exited"
    | "bad_request"
    | "internal_error"
    | "unknown";
  category: string;
  severity: "info" | "warning" | "error";
  title: string;
  details?: string | null;
  reason?: string | null;
  provider: string;
  account?: string | null;
  resetAt?: string | null;
  evidence: {
    source: string;
    confidence: "confirmed" | "hint";
  };
  actions: FailureAction[];
  recoveryPolicy: string;
}

/** One `session-failure` notice: `active: false` means it just cleared. */
export interface SessionFailureNotice {
  terminalId: string;
  failure: ServedSessionFailure;
  active: boolean;
}

/** Active failures, keyed by terminal id. */
export type FailuresByTerminal = Record<string, ServedSessionFailure[]>;

/**
 * Apply one notice. An active notice replaces the terminal's failure of the
 * same kind (the runner keeps one per kind, under a stable id) or adds it; a
 * cleared one removes it. Pure; returns `prev` unchanged when nothing moved.
 */
export function applyFailureNotice(
  prev: FailuresByTerminal,
  notice: SessionFailureNotice,
): FailuresByTerminal {
  const current = prev[notice.terminalId] ?? [];
  const others = current.filter(
    (f) => f.id !== notice.failure.id && f.kind !== notice.failure.kind,
  );
  if (!notice.active) {
    if (others.length === current.length) return prev;
    const next = { ...prev };
    if (others.length === 0) delete next[notice.terminalId];
    else next[notice.terminalId] = others;
    return next;
  }
  return { ...prev, [notice.terminalId]: [...others, notice.failure] };
}

/** Replace one terminal's list wholesale (the initial `terminal_failures` read). */
export function setTerminalFailures(
  prev: FailuresByTerminal,
  terminalId: string,
  failures: ServedSessionFailure[],
): FailuresByTerminal {
  const next = { ...prev };
  if (failures.length === 0) delete next[terminalId];
  else next[terminalId] = failures;
  return next;
}

/** A failure the runner inferred rather than observed stated. */
export function isHint(failure: ServedSessionFailure): boolean {
  return failure.evidence.confidence === "hint";
}

/** One banner row: a live tab and one of its active failures. */
export interface FailureBannerEntry {
  tab: TerminalTab;
  failure: ServedSessionFailure;
}

/**
 * The rows the banner shows: every active failure of every tab on this page,
 * errors before warnings, in tab order. A failure whose terminal has no tab
 * here belongs to another page and is not shown.
 */
export function failureBannerEntries(
  tabs: TerminalTab[],
  byTerminal: FailuresByTerminal,
): FailureBannerEntry[] {
  const rank = (f: ServedSessionFailure) =>
    f.severity === "error" ? 0 : f.severity === "warning" ? 1 : 2;
  const rows: FailureBannerEntry[] = [];
  for (const tab of tabs) {
    for (const failure of byTerminal[tab.id] ?? []) rows.push({ tab, failure });
  }
  return rows
    .map((row, i) => ({ row, i }))
    .sort((a, b) => rank(a.row.failure) - rank(b.row.failure) || a.i - b.i)
    .map(({ row }) => row);
}

/** What the resume verifier's tab flags say to report to the runner. */
export interface ResumeReports {
  /** Tabs whose resume just failed: report `terminal_report_resume_failure`. */
  failed: string[];
  /**
   * Tabs whose failed resume just verified — the flag cleared and no retry is
   * in flight: report `terminal_report_resume_verified`, the evidence that
   * ends the failure.
   */
  verified: string[];
}

/**
 * Diff the verifier's `resumeFailed` flags against those already reported.
 * `reported` holds the tabs currently reported as failed; the caller updates
 * it from the result. A retry clears `resumeFailed` BEFORE it verifies and
 * sets `isReconnecting` while it runs, so a cleared flag only counts as
 * verified once `isReconnecting` is off again — a retry in flight is not
 * evidence.
 */
export function resumeReports(tabs: TerminalTab[], reported: ReadonlySet<string>): ResumeReports {
  const failed: string[] = [];
  const verified: string[] = [];
  for (const tab of tabs) {
    if (tab.resumeFailed) {
      if (!reported.has(tab.id)) failed.push(tab.id);
    } else if (reported.has(tab.id) && !tab.isReconnecting) {
      verified.push(tab.id);
    }
  }
  return { failed, verified };
}

/** Human labels for the actions the banner can name. */
export const ACTION_LABELS: Record<FailureAction, string> = {
  retry: "Retry",
  login: "Log in",
  switch_account: "Switch account",
  new_session: "New session",
  resume: "Resume",
  none: "",
};

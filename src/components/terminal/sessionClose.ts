/**
 * Closing sessions from the Session Manager — D7 of plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`.
 *
 * Closing a session's tab ends its live conversation process: the transcript
 * survives and can be resumed, the in-flight context does not. So:
 *
 * - **Close** goes through `terminal_graceful_close`, the Tauri door onto
 *   `TerminalManager::graceful_exit` (`commands/session_close.rs`). Every
 *   outcome maps to a per-item result, and a refusal is reported as
 *   "not closed: <reason>" — it NEVER falls back to a kill.
 * - **Force close** is the page's plain close (`terminal_close`, which kills
 *   the pane tree). It is a separate action with its own confirmation.
 * - Each confirmation names every session and its activity word, and a row
 *   reading `Needs you` / `Working` / `Unknown` needs a second explicit tick
 *   ({@link closeNeedsSecondTick}).
 * - Every session that actually closed is recorded in the store's Recently
 *   closed list (`recordClosed`), with its resume command.
 *
 * The orchestration takes its effects as injected callbacks so it is testable
 * without Tauri.
 */

import { describeThrown } from "@/lib/utils";

import type { SessionOrgOp } from "./sessionOrgTypes";
import type { SessionActivity } from "./sessionStatusModel";

/** The activities whose close asks for a second explicit tick (D7). */
export const SECOND_TICK_ACTIVITIES: ReadonlySet<SessionActivity> = new Set<SessionActivity>([
  "needs-you",
  "working",
  "unknown",
]);

export function closeNeedsSecondTick(activity: SessionActivity): boolean {
  return SECOND_TICK_ACTIVITIES.has(activity);
}

/** `GracefulExitOutcome` as the Tauri command serialises it (`outcome` tag). */
export type GracefulCloseOutcome =
  | { outcome: "exited"; waited_ms: number; claude_pids: number[] }
  | {
      outcome: "exit_stuck";
      waited_ms: number;
      claude_pids: number[];
      last_probe_unreadable: boolean;
    }
  | { outcome: "refused"; reason: string; claude_pids: number[] }
  | { outcome: "no_live_claude" }
  | { outcome: "probe_unavailable"; detail: string }
  | { outcome: "write_failed"; error: string; claude_pids: number[] }
  | { outcome: "close_refused"; waited_ms: number; claude_pids: number[]; reason: string }
  | { outcome: "close_outcome_unknown"; waited_ms: number; claude_pids: number[]; detail: string };

/** One session's close, as the dialog lists it afterwards. */
export interface CloseItemResult {
  sessionId: string;
  title: string;
  /**
   * `closed` — the tab is gone. `not-closed` — the tab was left as it was.
   * `unknown` — the close was attempted and its result is not known.
   */
  state: "closed" | "not-closed" | "unknown";
  detail: string;
}

/** What a close needs to know about one row. */
export interface CloseItem {
  sessionId: string;
  title: string;
  /** The live tab in this window; `null` = nothing here to close. */
  tabId: string | null;
  /** The organization-store key the close is recorded under. */
  sessionKey: string;
  /** The resume command recorded with the close; `null` = not resumable. */
  resumeCmd: string | null;
}

function seconds(ms: number): string {
  return `${Math.max(0, Math.round(ms / 1000))}s`;
}

/** A per-item result for one graceful-close outcome — never "closed" unless it was. */
export function describeGracefulOutcome(raw: unknown): Pick<CloseItemResult, "state" | "detail"> {
  const o = raw as Partial<GracefulCloseOutcome> | null;
  switch (o?.outcome) {
    case "exited":
      return {
        state: "closed",
        detail: `Closed — claude exited after ${seconds(o.waited_ms ?? 0)}`,
      };
    case "exit_stuck":
      return {
        state: "not-closed",
        detail: `not closed: claude was still running after ${seconds(o.waited_ms ?? 0)} — nothing was killed`,
      };
    case "refused":
      return { state: "not-closed", detail: `not closed: ${o.reason ?? "refused"}` };
    case "no_live_claude":
      return {
        state: "not-closed",
        detail: "not closed: no Claude is running in this pane — Force close closes the tab",
      };
    case "probe_unavailable":
      return {
        state: "not-closed",
        detail: `not closed: the process table could not be read (${o.detail ?? "no detail"})`,
      };
    case "write_failed":
      return {
        state: "not-closed",
        detail: `not closed: /exit could not be typed (${o.error ?? "no detail"})`,
      };
    case "close_refused":
      return { state: "not-closed", detail: `not closed: ${o.reason ?? "the close refused"}` };
    case "close_outcome_unknown":
      return {
        state: "unknown",
        detail: `claude exited, but whether the tab closed is unknown (${o.detail ?? "no detail"})`,
      };
    default:
      return { state: "unknown", detail: `unrecognised close outcome: ${JSON.stringify(raw)}` };
  }
}

export interface CloseEffects {
  /** `terminal_graceful_close`; rejects when the runner has no such terminal. */
  gracefulClose: (tabId: string) => Promise<unknown>;
  /** The page's plain close — kills the pane tree (Force close only). */
  forceClose: (tabId: string) => void;
  /** Drop a tab the runner already closed from this window's tab list. */
  dropTab: (tabId: string) => void;
  /** One batch of `recordClosed` ops for the sessions that closed. */
  recordClosed: (ops: SessionOrgOp[]) => void;
  now?: () => number;
}

function recordOps(items: readonly CloseItem[], now: number): SessionOrgOp[] {
  return items.map((item) => ({
    op: "recordClosed",
    sessionKey: item.sessionKey,
    title: item.title,
    closedAt: now,
    resumeCmd: item.resumeCmd,
  }));
}

function nothingHere(item: CloseItem): CloseItemResult {
  return {
    sessionId: item.sessionId,
    title: item.title,
    state: "not-closed",
    detail: "not closed: it has no live tab in this window",
  };
}

/**
 * Close `items` gracefully, all at once (each waits on its own pane), and
 * answer one result per item in input order. Closed tabs are dropped from
 * the window and recorded as recently closed in ONE batch.
 */
export async function closeSessionsGracefully(
  items: readonly CloseItem[],
  effects: CloseEffects,
): Promise<CloseItemResult[]> {
  const results = await Promise.all(
    items.map(async (item): Promise<CloseItemResult> => {
      if (!item.tabId) return nothingHere(item);
      try {
        const outcome = await effects.gracefulClose(item.tabId);
        return {
          sessionId: item.sessionId,
          title: item.title,
          ...describeGracefulOutcome(outcome),
        };
      } catch (e) {
        return {
          sessionId: item.sessionId,
          title: item.title,
          state: "not-closed",
          detail: `not closed: ${describeThrown(e, "terminal_graceful_close failed")}`,
        };
      }
    }),
  );
  const closed = items.filter((_, i) => results[i].state === "closed");
  for (const item of closed) if (item.tabId) effects.dropTab(item.tabId);
  if (closed.length > 0) effects.recordClosed(recordOps(closed, (effects.now ?? Date.now)()));
  return results;
}

/** Force close `items` (the kill path) and record each one closed. */
export function forceCloseSessions(
  items: readonly CloseItem[],
  effects: CloseEffects,
): CloseItemResult[] {
  const results = items.map((item): CloseItemResult => {
    if (!item.tabId) return nothingHere(item);
    effects.forceClose(item.tabId);
    return {
      sessionId: item.sessionId,
      title: item.title,
      state: "closed",
      detail: "Force closed — the pane was killed",
    };
  });
  const closed = items.filter((_, i) => results[i].state === "closed");
  if (closed.length > 0) effects.recordClosed(recordOps(closed, (effects.now ?? Date.now)()));
  return results;
}

/** One line summarising a batch — never a single "done" for a partial batch. */
export function summarizeCloseResults(results: readonly CloseItemResult[]): string {
  const closed = results.filter((r) => r.state === "closed").length;
  const notClosed = results.filter((r) => r.state === "not-closed").length;
  const unknown = results.filter((r) => r.state === "unknown").length;
  const parts = [`${closed} of ${results.length} closed`];
  if (notClosed) parts.push(`${notClosed} not closed`);
  if (unknown) parts.push(`${unknown} unknown`);
  return parts.join(" · ");
}

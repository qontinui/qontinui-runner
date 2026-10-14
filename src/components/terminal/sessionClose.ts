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
 *   the pane tree). It is a separate action with its own confirmation, and it
 *   reports what the close actually did ({@link describeForceCloseOutcome}):
 *   a kill is "closed" only once `terminal_close` answered; a Conductor
 *   worker's tab is a view, so closing it kills nothing.
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

/** One session a close confirmation would close, as of this render. */
export interface CloseTarget {
  sessionId: string;
  title: string;
  activity: SessionActivity;
  /** The activity word the card shows (`Needs you`, `Exited — error (code 2)`, …). */
  activityWord: string;
  /** Has a tab here — otherwise it is listed but nothing is closed. */
  closable: boolean;
  /**
   * Why the row is offered for closing (`Finished · PRs green`, `Exited 0`);
   * set by the Ready-to-close review, absent for a close the operator asked for.
   */
  reason?: string;
}

/** The tick key: a tick covers one session in ONE activity. */
export function closeTickKey(target: Pick<CloseTarget, "sessionId" | "activity">): string {
  return `${target.sessionId}:${target.activity}`;
}

/** Targets that would close and still need their second tick. */
export function untickedRiskyTargets(
  targets: readonly CloseTarget[],
  ticks: ReadonlySet<string>,
): CloseTarget[] {
  return targets.filter(
    (t) => t.closable && closeNeedsSecondTick(t.activity) && !ticks.has(closeTickKey(t)),
  );
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
  /** The session's tab in this window; `null` = nothing here to close. */
  tabId: string | null;
  /**
   * Whether that tab's process is still running. An exited tab has no
   * conversation left to end, so closing it only removes the tab.
   */
  tabAlive: boolean;
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

/**
 * What the page's close path (`useTerminalManager().closeTerminalReporting`)
 * observed doing to one tab.
 *
 * - `killed` — `terminal_close` answered success: the pane tree was killed.
 * - `worker-view` — a Conductor worker's tab (`sessionBacked`): the tab was
 *   hidden and nothing was killed; the Conductor owns the worker.
 * - `no-process` — a tab with no PTY (a plan tab): removed, nothing to kill.
 * - `unconfirmed` — the kill was requested and never confirmed (the command
 *   rejected, answered failure, or the close path returned no outcome).
 */
export type TerminalCloseOutcome =
  | { kind: "killed" }
  | { kind: "worker-view" }
  | { kind: "no-process" }
  | { kind: "unconfirmed"; detail: string };

/** A per-item result for one Force close outcome — "closed" only for an observed kill. */
export function describeForceCloseOutcome(
  raw: TerminalCloseOutcome | void | undefined,
): Pick<CloseItemResult, "state" | "detail"> {
  switch (raw?.kind) {
    case "killed":
      return { state: "closed", detail: "Force closed — the pane was killed" };
    case "worker-view":
      return {
        state: "not-closed",
        detail:
          "not closed: a Conductor worker's tab is a view; nothing was killed (the tab was hidden)",
      };
    case "no-process":
      return {
        state: "not-closed",
        detail: "not closed: the tab had no process; nothing was killed (the tab was removed)",
      };
    case "unconfirmed":
      return { state: "unknown", detail: `kill requested; not confirmed (${raw.detail})` };
    default:
      return { state: "unknown", detail: "kill requested; not confirmed (no outcome reported)" };
  }
}

export interface CloseEffects {
  /** `terminal_graceful_close`; rejects when the runner has no such terminal. */
  gracefulClose: (tabId: string) => Promise<unknown>;
  /**
   * The page's plain close — kills the pane tree (Force close only) — and
   * what it observed. A `void` answer is read as unconfirmed.
   */
  forceClose: (tabId: string) => Promise<TerminalCloseOutcome> | void;
  /**
   * Drop a tab the runner already closed from this window's tab list.
   *
   * ORDERING: the graceful path calls this only AFTER `gracefulClose` for that
   * tab has resolved to a `closed` result — never before, never for any other
   * result (pinned by `sessionClose.test.ts`, "drops a tab only after its
   * graceful close answered closed"). So when it goes through the page's close
   * path, the runner has already ended the pane and that path's own
   * `terminal_close` has nothing left to kill.
   */
  dropTab: (tabId: string) => unknown;
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

/** An exited tab: nothing to /exit and nothing to kill — the tab is removed. */
function removeExited(item: CloseItem, effects: CloseEffects): CloseItemResult {
  if (item.tabId) effects.dropTab(item.tabId);
  return {
    sessionId: item.sessionId,
    title: item.title,
    state: "closed",
    detail: "Closed — the session had already exited; its tab was removed",
  };
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
      if (!item.tabAlive) return removeExited(item, effects);
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
  for (const item of closed) if (item.tabId && item.tabAlive) effects.dropTab(item.tabId);
  if (closed.length > 0) effects.recordClosed(recordOps(closed, (effects.now ?? Date.now)()));
  return results;
}

/**
 * Force close `items` (the kill path), all at once, and answer one result per
 * item in input order. Only an observed kill counts as closed, and only the
 * sessions that closed are recorded as recently closed (in ONE batch).
 */
export async function forceCloseSessions(
  items: readonly CloseItem[],
  effects: CloseEffects,
): Promise<CloseItemResult[]> {
  const results = await Promise.all(
    items.map(async (item): Promise<CloseItemResult> => {
      if (!item.tabId) return nothingHere(item);
      if (!item.tabAlive) return removeExited(item, effects);
      let outcome: TerminalCloseOutcome | void;
      try {
        outcome = await effects.forceClose(item.tabId);
      } catch (e) {
        outcome = { kind: "unconfirmed", detail: describeThrown(e, "terminal_close failed") };
      }
      return { sessionId: item.sessionId, title: item.title, ...describeForceCloseOutcome(outcome) };
    }),
  );
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

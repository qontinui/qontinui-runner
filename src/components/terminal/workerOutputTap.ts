/**
 * The page-scope tracking feed for Conductor workers.
 *
 * Phase 2b (`2026-09-12-consolidate-local-orchestration-onto-conductor`,
 * qontinui-runner#1553) gave a worker a grid CELL: `WorkerSessionCell` attaches
 * to the session through `useAiSession` and renders its conversation, diffs and
 * steering. Everything the grid draws AROUND that cell still ran off
 * `useSessionStateTracking`, and that hook has exactly one feed — the
 * `terminal-output` tap in `TerminalSessionContext`.
 *
 * A worker has no PTY and emits no `terminal-output` ever, so for a worker tab
 * `lastOutputTimeRef` stayed 0, the sparkline stayed flat, `lastOutputLines`
 * stayed empty and `sessionStates[tabId]` stayed `"idle"` for the session's
 * whole life. Every surface that reads those — the zone label's state chip, the
 * `CompactZoneCard` a virtualized or compact zone renders INSTEAD of the cell,
 * the StatusStrip pills, the `focusNextNeedsInput` / `focusNextError` cyclers,
 * zone output search — therefore reported a working worker as an idle, silent
 * terminal. That is the same lie Phase 2b set out to remove, one layer up from
 * the cell it fixed (`ux-priorities` honesty).
 *
 * This module is the mapping half of the second feed: `ai-output` →
 * `handleWorkerOutput`, `claude-session-state` → `applyWorkerSessionState`.
 * Pure and dependency-free, so the routing contract is unit-testable in the
 * runner's `node` vitest environment without mounting React or a runner.
 */

import type { AiSessionState } from "@qontinui/shared-types";
import type { SessionState } from "./useZoneLayout";

/** The `ai-output` Tauri event, as much of it as this feed reads. */
export interface WorkerAiOutputPayload {
  line?: string | null;
  source?: string | null;
  taskRunId?: string | null;
}

/** The `claude-session-state` Tauri event, as much of it as this feed reads. */
export interface WorkerSessionStatePayload {
  taskRunId?: string | null;
  state?: string | null;
}

/**
 * `ai-output` sources this feed deliberately DROPS.
 *
 * - `user_message` is the echo of steering WE sent. Counting it as worker
 *   output would move `lastOutputTime` on the operator's own keystroke and make
 *   a wedged worker look alive the moment someone typed at it.
 * - `status` is the runner's own heartbeat line (`⏳ AI is working... (30s)`),
 *   emitted on a timer rather than by the worker. Feeding it would refresh
 *   `lastOutputTime` forever, so the 60 s staleness sweep could never mark a
 *   stuck worker stale — it would report liveness it never measured
 *   (`verification-and-evidence` `a-status-signal-must-observe-the-state-it-names`).
 *
 * Everything else — assistant text, `tool_activity` ("Reading foo.rs"),
 * `system_note` — is the worker actually doing something, and `tool_activity`
 * in particular is the most useful line a compact card can show.
 */
export const DROPPED_AI_OUTPUT_SOURCES: ReadonlySet<string> = new Set(["user_message", "status"]);

/**
 * The text this payload contributes to tracking, or `null` when it contributes
 * nothing. Pure.
 */
export function workerTextFromAiOutput(payload: WorkerAiOutputPayload | null): string | null {
  const line = payload?.line;
  if (typeof line !== "string" || line.length === 0) return null;
  const source = payload?.source;
  if (typeof source === "string" && DROPPED_AI_OUTPUT_SOURCES.has(source)) return null;
  return line;
}

/**
 * Map the worker's AUTHORITATIVE session state onto the grid's state
 * vocabulary.
 *
 * Deliberately not `detectSessionState`: that runs the PTY approval-prompt
 * regexes over raw terminal bytes, and a worker's assistant markdown quoting
 * the words "do you want to proceed" would invent a `needs-input` the worker is
 * not in — an affordance (quick-approve) that would then write `y` into a PTY
 * that does not exist. `claude-session-state` reports the real state, so the
 * state feed reads it and the text feed never touches state at all.
 *
 * `needs-input` is unreachable on purpose: a stream-json worker has no
 * interactive prompt, and the Conductor steers it through `send_user_message`.
 * Returning `null` means "this event says nothing about the tab's state" — the
 * caller then leaves the previous value alone rather than writing a default
 * (`verification-and-evidence` `unknown-must-not-render-as-a-default`).
 */
export function workerSessionStateFor(state: string | null | undefined): SessionState | null {
  switch (state as AiSessionState | null | undefined) {
    case "processing":
    case "interrupting":
      return "working";
    case "ready":
      return "idle";
    case "connecting":
    case "initializing":
    case "restoring":
      return "idle";
    case "closed":
      return "completed";
    case "error":
      return "error";
    // `not_found` is the SessionManager not knowing this id. That is not
    // "finished" — it is unknown — and a `completed` here would light up the
    // compact card's Restart affordance for a worker nobody can account for.
    case "not_found":
    case "disconnected":
      return null;
    default:
      return null;
  }
}

/**
 * Per-tab text accumulator for the `ai-output` feed, drained once per animation
 * frame by the subscriber.
 *
 * Mirrors what `TerminalOutputCoalescer` does for the PTY tap and exists for the
 * same reason: a worker streams a line at a time, and calling `handleOutput`
 * once per line would re-render every consumer of `sessionStates` at the
 * provider's event rate. Simpler than its PTY sibling because the wire already
 * carries decoded text — there is no `TextDecoder` to keep per stream.
 */
export class WorkerOutputCoalescer {
  private pending = new Map<string, string>();

  push(tabId: string, text: string): void {
    const prior = this.pending.get(tabId);
    // The lines arrive already split, so re-join with the separator every
    // downstream reader (`nextOutputLines`) splits on.
    this.pending.set(tabId, prior === undefined ? text : `${prior}\n${text}`);
  }

  /** Take everything buffered, leaving the coalescer empty. */
  drain(): Array<[string, string]> {
    if (this.pending.size === 0) return [];
    const out = [...this.pending.entries()];
    this.pending.clear();
    return out;
  }

  /** Drop buffered text for tabs that are no longer on this page. */
  retain(tabIds: ReadonlySet<string>): void {
    for (const id of this.pending.keys()) {
      if (!tabIds.has(id)) this.pending.delete(id);
    }
  }

  get size(): number {
    return this.pending.size;
  }
}

/**
 * `taskRunId -> tabId` for the worker tabs on one page.
 *
 * A worker tab's id EQUALS its task run id today (`dispatch_subtask` pins the
 * CLI session id to the run id, and `workerTabFromRecord` keys the tab by the
 * record's `terminalId`), but this feed routes through the declared
 * `taskRunId` rather than that coincidence — the same reason `isWorkerRecord`
 * keys on `taskRunId` instead of `terminalId === claudeSessionId`. Pure.
 */
export function workerTabsByTaskRun(
  tabs: readonly { id: string; taskRunId?: string; sessionBacked?: boolean }[],
): Map<string, string> {
  const byRun = new Map<string, string>();
  for (const tab of tabs) {
    if (!tab.sessionBacked || !tab.taskRunId) continue;
    byRun.set(tab.taskRunId, tab.id);
  }
  return byRun;
}

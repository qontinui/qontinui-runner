/**
 * Terminal Sessions Registry
 *
 * Module-level snapshot of the runner's terminal sessions (the tabs owned by
 * `useTerminalManager`) plus their session-state-machine status, kept current
 * by `TerminalPageInner` on every render. UI Bridge IPC handlers read from
 * this registry to answer `GET /ui-bridge/control/terminal-sessions[/{id}]`
 * — endpoints that need a frontend-owned view of the tab list without
 * coupling to React mount order.
 *
 * Why a module singleton: the IPC dispatch path (`useUIBridgeEventHandler`)
 * lives outside the terminal-page subtree, so it can't reach into
 * `TerminalSessionContext` directly. Rather than threading the context through
 * every sub-hook or fanning out a custom DOM event, the page publishes its
 * snapshot here and the new `useTerminalsEvents` hook reads it. The
 * registry mirrors how `instanceStorage[ACTIVE_TAB_STORAGE_KEY]` is used
 * by `usePageEvents.ts` (`tabs_list`) — a tiny module-scope leak that
 * lets stateless handlers serve frontend-owned data.
 */

/** Mirrors `SessionState` from `./components/terminal/useZoneLayout.ts`. */
export type TerminalSessionState =
  | "idle"
  | "working"
  | "needs-input"
  | "completed"
  | "error";

/**
 * A single terminal session entry as published to the UI Bridge.
 *
 * `taskRunId` / `claudeSessionId` are `null` for tabs that have not yet
 * captured a Claude Code session id (fresh AI tabs in the bootstrap window,
 * or plain pwsh tabs that never will). Callers correlate `tabId` ↔
 * `taskRunId` by polling `/ui-bridge/control/terminal-sessions/{id}`.
 */
export interface TerminalSessionEntry {
  /** Runner-internal tab id (matches `tab.id` from `useTerminalManager`). */
  id: string;
  /** Display title (== `holder_name` for file-lock events). */
  title: string;
  /** Claude Code task-run id (== `claudeSessionId` today). `null` until
   *  captured by `useTabSessionIdCapture` (~JSONL first write). */
  taskRunId: string | null;
  /** Alias kept in lockstep with `taskRunId`; the two may diverge if the
   *  runner adopts a separate task-runs surface in the future. */
  claudeSessionId: string | null;
  /** Tab's working directory. Empty string when the tab has not reported
   *  one yet (preserves the `string` shape rather than threading nulls). */
  workingDir: string;
  /** Sourced from `useSessionStateTracking.sessionStates` for AI tabs;
   *  defaults to `"idle"` for plain PTY tabs that never register a state. */
  state: TerminalSessionState;
  /** True when the underlying PTY is still alive (false after exit). */
  isAlive: boolean;
  /** Process exit code, when known. `null` while the PTY is running. */
  exitCode: number | null;
  /** Tab type discriminator from `useTerminalManager`. Defaults to
   *  `"terminal"` when undefined. */
  type: "terminal" | "plan";
  /** Epoch ms when the tab was created. `null` when not yet recorded. */
  createdAt: number | null;
}

let snapshot: readonly TerminalSessionEntry[] = [];

/**
 * Replace the registry's snapshot. Called by `TerminalPageInner` on every
 * render that affects the published fields (tabs / session states).
 */
export function setTerminalSessions(next: readonly TerminalSessionEntry[]): void {
  snapshot = next;
}

/** Read the current snapshot. Returns the same array reference between
 *  publishes so callers can cheaply detect "nothing changed". */
export function getTerminalSessions(): readonly TerminalSessionEntry[] {
  return snapshot;
}

/** Look up a single entry by `tab.id`. Returns `undefined` when no match
 *  exists — handlers turn that into HTTP 404. */
export function getTerminalSession(id: string): TerminalSessionEntry | undefined {
  return snapshot.find((entry) => entry.id === id);
}

/** Reset for tests. Not exported through the package barrel. */
export function __resetTerminalSessionsForTest(): void {
  snapshot = [];
}

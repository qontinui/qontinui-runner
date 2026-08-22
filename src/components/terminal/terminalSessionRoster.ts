/**
 * Terminal-session roster — the layout-independent census automation reads via
 * `POST /ui-bridge/control/page/read-value
 * {"selector":"[data-page-element=terminal-session-roster]"}`.
 *
 * A leaf module (zero imports) so the payload contract is unit-testable: the
 * runner's vitest config is `environment: "node"`, and `TerminalPage` cannot be
 * imported there.
 *
 * ## One spelling of liveness, not a new one
 *
 * The roster row deliberately reuses `state` / `isAlive` / `exitCode` — the
 * EXACT field names, sources and defaults the same component already publishes
 * as `TerminalSessionEntry` (`TerminalPage.tsx`, served by
 * `GET /control/terminal-sessions`). The roster was the one census on this page
 * that carried no liveness at all, so an exited PTY read `isReconnecting:
 * false` like every live session and a verifier asking "is this terminal
 * usable?" got a confident yes for a dead process. The fix is to surface the
 * liveness the page already tracks — inventing a fourth spelling (`dead`,
 * `liveState`, …) would rebuild, at the naming layer, the very
 * two-counters-from-two-models defect the StatusStrip fix exists to close.
 */

/** The subset of a `TerminalTab` the roster projects. */
export interface RosterTab {
  id: string;
  title: string;
  claudeSessionId?: string | null;
  isReconnecting?: boolean;
  /** `false` once the page's exit handler recorded the PTY's exit. */
  isAlive?: boolean;
  exitCode?: number | null;
}

/** Mirrors `TerminalSessionState` from `@/lib/terminal-sessions-registry`. */
export type RosterSessionState = "idle" | "working" | "needs-input" | "completed" | "error";

/** One roster row. */
export interface RosterRow {
  claudeSessionId: string | null;
  terminalId: string;
  zoneIndex: number;
  title: string;
  isReconnecting: boolean;
  /**
   * Session-state-machine status, same source and same `"idle"` default as
   * `TerminalSessionEntry.state`. `"error"` / `"completed"` are what the page's
   * own exit handler writes for a dead PTY.
   */
  state: RosterSessionState;
  /** True while the PTY is alive; `false` after exit. Same spelling as
   *  `TerminalSessionEntry.isAlive`. */
  isAlive: boolean;
  /** The exited process's code when one was observed; `null` while running. */
  exitCode: number | null;
}

/**
 * Project the page's tabs (plus the live zone assignments and the session-state
 * map) into the roster.
 *
 * Sourced from `tabs`, never from mounted zones, so a virtualized or unzoned
 * session is still listed — with `zoneIndex: -1` when it holds no zone.
 *
 * NOTE for the caller: `sessionStates` is a real input, so the memo that wraps
 * this MUST list it as a dependency. Reading liveness through a memo keyed only
 * on `[tabs, assignments]` would publish a stale row — a dead PTY still
 * rendering alive, which is the failure this roster field exists to prevent.
 */
export function buildTerminalSessionRoster(
  tabs: readonly RosterTab[],
  assignments: Record<string | number, string | undefined>,
  sessionStates: Record<string, RosterSessionState | undefined>,
): RosterRow[] {
  return tabs.map((t) => {
    const zoneIndex = Number(
      Object.entries(assignments ?? {}).find(([, id]) => id === t.id)?.[0] ?? -1,
    );
    return {
      claudeSessionId: t.claudeSessionId ?? null,
      terminalId: t.id,
      zoneIndex,
      title: t.title,
      isReconnecting: Boolean(t.isReconnecting),
      state: sessionStates?.[t.id] ?? "idle",
      isAlive: Boolean(t.isAlive),
      exitCode: t.exitCode ?? null,
    };
  });
}

/**
 * `get-roster-resync-stats` — a read-only UI Bridge action on the
 * `terminal-page` component that reports how this window's cross-window
 * roster resyncs ended (plan
 * 2026-11-23-runner-terminal-page-popout-ui-bridge-and-resync-lag, D3).
 *
 * Each foreign roster save another window announces is re-read here until the
 * store holds the announced digest (`useTerminalPages`' resync effect), and
 * each such event records one entry in `terminalPagesSync`'s ring. This action
 * serves that ring, this window's label and the current budget, so the
 * cross-webview `localStorage` lag is a value the UI Bridge returns rather
 * than a console line nobody collects. With per-window component-action
 * routing it is read from a pop-out as
 * `POST /ui-bridge/control/component/terminal-page/action/get-roster-resync-stats?windowLabel=term-N`.
 *
 * `entries: []` beside a `since` means "no foreign event since then", never a
 * zero lag; `since: null` means no roster listener has mounted in this window,
 * so nothing could have been recorded.
 *
 * A pure factory, kept out of the 1,900-line `TerminalPage.tsx`, so it is
 * unit-testable under the node vitest environment (the
 * `createPlainTerminalAction` precedent). It is a `function` declaration
 * returning an object literal on purpose: the effect- and signature-coverage
 * walk (`src/lib/ui-bridge/action-effect-coverage.test.ts`) follows exactly
 * that shape into a factory.
 */

import type { EffectSignature } from "@qontinui/ui-bridge";
import { readResyncLog, type ResyncEntry } from "./terminalPagesSync";
import { GET_ROSTER_RESYNC_STATS_SIGNATURE } from "./terminalPageSignatures";
import { ROSTER_RESYNC_BUDGET } from "./useTerminalPages";

/** The action id agents invoke on the `terminal-page` component. */
export const ROSTER_RESYNC_STATS_ACTION_ID = "get-roster-resync-stats";

/** What the action returns. */
export interface RosterResyncStats {
  /** The Tauri label of the window that answered. */
  windowLabel: string;
  /** When this window began recording; `null` before any listener mounted. */
  since: number | null;
  /** The resync budget in force. */
  budget: { retries: number; retryMs: number; finalRereadMs: number };
  /** The last resync outcomes, oldest first. */
  entries: ResyncEntry[];
}

/** Minimal shape of a UI Bridge component action (the SDK's ComponentActionDef). */
export interface RosterResyncStatsActionDef {
  id: string;
  label: string;
  description: string;
  /** Required, as on `PlainTerminalActionDef`: a factory-built action. */
  effect: "read" | "write" | "destructive";
  signature?: EffectSignature;
  handler: () => RosterResyncStats;
}

/** Read the stats as the action returns them. */
export function readRosterResyncStats(windowLabel: string): RosterResyncStats {
  const { since, entries } = readResyncLog();
  return {
    windowLabel,
    since,
    budget: {
      retries: ROSTER_RESYNC_BUDGET.retries,
      retryMs: ROSTER_RESYNC_BUDGET.retryMs,
      finalRereadMs: ROSTER_RESYNC_BUDGET.finalRereadMs,
    },
    entries,
  };
}

/**
 * Build the `get-roster-resync-stats` action def.
 *
 * @param getWindowLabel this window's Tauri label, read when the action runs.
 */
export function buildRosterResyncStatsAction(
  getWindowLabel: () => string,
): RosterResyncStatsActionDef {
  return {
    id: "get-roster-resync-stats",
    label: "Get Roster Resync Stats",
    description:
      "Return how this window's cross-window page-roster resyncs ended: " +
      "{ windowLabel, since, budget: { retries, retryMs, finalRereadMs }, entries: " +
      "[{ source, receiver, digest, startedAt, elapsedMs, attempts, outcome }] } with outcome " +
      "matched | matched-late | exhausted | unreadable | superseded | own-save (last 50, " +
      "oldest first). entries [] beside since means no foreign save since then; since null " +
      "means nothing was recorded yet. Pass ?windowLabel=term-N to read a pop-out.",
    // `read` — returns an in-memory log. A query: nothing changes.
    effect: "read",
    signature: GET_ROSTER_RESYNC_STATS_SIGNATURE,
    handler: () => readRosterResyncStats(getWindowLabel()),
  };
}

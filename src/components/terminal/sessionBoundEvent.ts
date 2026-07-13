/**
 * `session-bound` event — the backend binder's tab-stamp channel.
 *
 * The continuous reconcile binder (`src-tauri/src/session/reconcile.rs`) binds
 * hand-typed / absolute-path provider launches into the DURABLE registry, but
 * the frontend tab object never learned the id — so the durability marker
 * (`sessionDurability.ts`, the "ephemeral" tag) dishonestly read non-durable
 * for a session that restores fine. The backend now emits one `session-bound`
 * event per bind (see `SESSION_BOUND_EVENT` / `SessionBoundPayload` in
 * `reconcile.rs` — the camelCase wire shape here is that contract); the
 * frontend stamps `tab.claudeSessionId` so the tag flips within the same poll
 * tick.
 *
 * This module is the PURE core (payload shape + tab-match decision) so the
 * decision table is unit-testable in the node vitest env without React or a
 * Tauri mock; `useSessionBoundEvents.ts` does the listening and applying.
 */

import type { TerminalTab } from "./useTerminalManager";

/** Wire payload of one `session-bound` event (camelCase per serde rename). */
export interface SessionBoundPayload {
  /** Hosting terminal id — equals the frontend tab id. */
  terminalId: string;
  /** The bound provider session id (registry key). */
  sessionId: string;
  /** Config dir the bind resolved, or "" when not yet known. */
  configDir: string;
  /** Evidence grade: "authoritative" | "observed" | "reconciled". */
  origin: string;
  /** Whether the registry record was written confirmed. */
  confirmed: boolean;
}

/** The tab update a `session-bound` event resolves to. */
export interface SessionBoundUpdate {
  tabId: string;
  claudeSessionId: string;
  /** Undefined when the bind didn't resolve a config dir. */
  claudeConfigDir: string | undefined;
}

/**
 * Decide the tab update for one `session-bound` event.
 *
 * Returns `null` (no-op) when:
 *  - no tab matches the payload's terminal id (tab closed / other window), or
 *  - the tab ALREADY has a `claudeSessionId` — the launch-menu pin / resume
 *    path / a prior event got there first and stays authoritative for the tab;
 *    the backend registry (which the binder already wrote) is the truth for
 *    restore either way, so the frontend never re-stamps.
 *
 * Pure + exported for unit tests.
 */
export function applySessionBound(
  tabs: ReadonlyArray<Pick<TerminalTab, "id"> & { claudeSessionId?: string }>,
  payload: SessionBoundPayload,
): SessionBoundUpdate | null {
  if (!payload.terminalId || !payload.sessionId) return null;
  const tab = tabs.find((t) => t.id === payload.terminalId);
  if (!tab) return null;
  if (tab.claudeSessionId) return null;
  return {
    tabId: tab.id,
    claudeSessionId: payload.sessionId,
    claudeConfigDir: payload.configDir || undefined,
  };
}

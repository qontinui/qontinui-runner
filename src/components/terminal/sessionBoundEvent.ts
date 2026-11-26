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
  /**
   * Did the PROVIDER report this id about itself (its SessionStart hook), or
   * did the runner infer it? Only a self-report may CORRECT an id a tab already
   * holds — see `applySessionBound`. Optional on the wire so an older backend
   * that omits it is read as the safe value, `false`.
   */
  providerReported?: boolean;
}

/**
 * The form two spellings of one config dir share, mirroring Rust's
 * `config_dir_compare_key` (`session_archive/discovery.rs`): trimmed,
 * backslashes folded to `/`, trailing slashes dropped, and lowercased only for
 * a Windows-style drive path (the frontend has no platform flag; a drive
 * letter is the same evidence). Compare-only, never a path to open.
 */
export function configDirCompareKey(configDir: string): string {
  const p = configDir.trim().replace(/\\/g, "/").replace(/\/+$/, "");
  return /^[a-z]:(\/|$)/i.test(p) ? p.toLowerCase() : p;
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
 * A config dir the payload does not carry leaves the tab's existing one alone
 * — an omission is not a claim of absence, and the update is applied by spread.
 *
 * Returns `null` (no-op) when:
 *  - no tab matches the payload's terminal id (tab closed / other window),
 *  - the tab already holds the SAME id and the payload carries no config dir,
 *    or the one the tab already holds (nothing to change), or
 *  - the tab holds a DIFFERENT id and the bind is not provider-reported — the
 *    launch-menu pin / resume path stays put, and no runner INFERENCE may
 *    overwrite it.
 *
 * A payload for the SAME id with a non-empty config dir that differs from the
 * tab's re-stamps ONLY the config dir (the session id is returned unchanged).
 * That is the runner's config-dir repair (`reconcile/config_dir_repair.rs`):
 * the record's account was corrected from the pane's own provider row, and the
 * tab must follow it, or the next restore types the wrong account's wrapper.
 * It is runner-inferred, so it is not gated on `providerReported`; it moves no
 * session id, which is what that gate protects.
 *
 * It DOES re-stamp when the tab holds a different id and the provider itself
 * reported the new one. The previous rule bailed on the mere PRESENCE of a
 * `claudeSessionId`, which made the spawn-time PREDICTION permanent: the runner
 * stamps the tab with the `--session-id` it passes the provider, and whenever
 * the provider adopts a different id instead (every resume of a pre-existing
 * session, any rebind of a live session onto a new PTY) the tab kept the
 * prediction forever. The session-info dropdown reads this field, so it queried
 * an id no record was ever written under and rendered
 * `unavailable — session_not_found` while the store held a complete projection
 * for the session actually running in that PTY. Measured live 2026-08-29: zone
 * 1's tab held `a20acdbb…`, its terminal `ecb3d767` was bound to `44aadb3e…`,
 * and the dropdown showed nothing for five landed PRs.
 *
 * The gate is `providerReported`, NOT the `origin` grade, and the difference is
 * load-bearing: reconcile's rung-2 bind is graded `authoritative` as well, but
 * its id is lifted from the anchor process's typed `--session-id` — the
 * runner's own prediction. Gating on grade would let that bind overwrite a true
 * id with the guess, reinstating this defect in the opposite direction. Only
 * the provider's SessionStart hook reports an id about itself, and only it
 * corrects.
 *
 * Pure + exported for unit tests.
 */
export function applySessionBound(
  tabs: ReadonlyArray<
    Pick<TerminalTab, "id"> & { claudeSessionId?: string; claudeConfigDir?: string }
  >,
  payload: SessionBoundPayload,
): SessionBoundUpdate | null {
  if (!payload.terminalId || !payload.sessionId) return null;
  const tab = tabs.find((t) => t.id === payload.terminalId);
  if (!tab) return null;
  if (tab.claudeSessionId === payload.sessionId) {
    if (!payload.configDir) return null;
    if (
      tab.claudeConfigDir &&
      configDirCompareKey(payload.configDir) === configDirCompareKey(tab.claudeConfigDir)
    ) {
      return null;
    }
    return {
      tabId: tab.id,
      claudeSessionId: tab.claudeSessionId,
      claudeConfigDir: payload.configDir,
    };
  }
  if (tab.claudeSessionId && payload.providerReported !== true) return null;
  return {
    tabId: tab.id,
    claudeSessionId: payload.sessionId,
    // An OMITTED config dir is not a claim that there isn't one. The caller
    // applies this update by spread, so passing `undefined` through would
    // ERASE a known account (and persist the erasure to `lastKnownSessionIds`)
    // — reachable now that a provider-reported bind can correct a tab that is
    // already populated. The empty string is the wire contract's "unknown", so
    // an unknown falls back to what the tab already holds. A bind that DOES
    // carry an account still overwrites: that is the correction working.
    claudeConfigDir: payload.configDir || tab.claudeConfigDir || undefined,
  };
}

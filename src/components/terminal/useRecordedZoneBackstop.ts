import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";
import {
  buildSessionOpenArgs,
  buildSessionSyncZoneArgs,
  planZoneReemits,
  resolveZoneIndex,
  type OpenRecordTab,
  type RecordedZoneLedger,
} from "./sessionRecordArgs";
import type { TerminalWorkMode } from "./terminalWorkMode";
import type { ZoneAssignments } from "./useZoneLayout";

const logger = createLogger("TerminalPage");

/** How long the assignments must hold still before a pass re-resolves them. */
export const ZONE_BACKSTOP_DEBOUNCE_MS = 300;

/**
 * Whether the page's restore has really finished for the zone backstop's
 * purposes. A drain-DEFERRED page is in `restoreCompletePages` too (its gate
 * opens so auto-save and auto-trim are not held for the drain), but restore
 * has read and placed nothing yet: its reconnected tabs still sit in
 * localStorage / auto-fill zones, and the deferred re-run will read the
 * durable record. So a deferred page is NOT complete here.
 */
export function zoneSyncRestoreComplete(
  restoreCompletePages: ReadonlySet<string>,
  restoreDeferredPages: ReadonlySet<string>,
  pageId: string,
): boolean {
  return restoreCompletePages.has(pageId) && !restoreDeferredPages.has(pageId);
}

interface RecordedZoneBackstopInput {
  workMode: TerminalWorkMode;
  /** The page's live zone assignments. */
  assignments: ZoneAssignments;
  tabs: Array<OpenRecordTab & { claudeSessionId?: string; claudeConfigDir?: string }>;
  pageId: string;
  /** What each session's durable record was last WRITTEN with. */
  recordedZones: RecordedZoneLedger;
  /**
   * The page's restore run has finished (`restoreCompletePages.has(pageId)`).
   * Until then the durable record is the SOURCE OF TRUTH the restore places
   * sessions from, and a reload's reconnected tabs sit in zones taken from
   * localStorage or creation-order auto-fill, so a first-seen sync would
   * overwrite the record restore is about to read. First-seen sessions are
   * therefore neither synced nor seeded until this is true. Callers compute
   * it with {@link zoneSyncRestoreComplete}. A page whose restore aborts and
   * is never retried stays not-complete, exactly as its auto-save gate does,
   * so its first-seen sessions wait for the page's next restore run.
   */
  restoreComplete: boolean;
}

/**
 * Zone re-resolution backstop. Three jobs, one mechanism:
 *
 *  1. Zone MOVE — a Claude session dragged between zones: the recorded
 *     `zoneIndex` must follow it (in both directions, including back to
 *     `UNZONED_INDEX` when the drag leaves the tab unassigned).
 *  2. Zone BIND-TIME STALENESS — the OPEN record is written the moment a tab
 *     binds its `claudeSessionId`, which is normally before
 *     `reconcileAssignments` has auto-filled that tab into a zone, so the
 *     record is written `-1` and then never revisited. `recordSessionOpen`
 *     seeds the ledger with the zone it WROTE, so the first observation here
 *     sees `-1 → 3` and corrects it.
 *  3. Zone of a record this page NEVER WROTE — a session created over
 *     `POST /terminals` is recorded by the backend at zone 0 (neither the
 *     spawn seam nor the typed-line sniff can see the grid), so a session
 *     rendered in zone 2 used to keep recorded zone 0 and restore into the
 *     wrong zone (measured 2026-11-08, coord finding `898efce4`). On its first
 *     observation the backstop calls `terminal_session_sync_zone`, which moves
 *     the recorded zone ONLY, and only for an OPEN record bound to this very
 *     terminal. It never re-asserts `terminal_session_record_open` for such a
 *     row: that refreshes `last_seen_at`, which is how ghost rows became
 *     immortal.
 *
 * `-1` is deliberately still a reachable recorded value: past the 9-zone
 * ceiling a live tab is genuinely unassigned, and nothing here clamps it to
 * zone 0. Debounced and keyed on `assignments`/`tabs`, so it runs only after
 * the assignment it is re-resolving has actually settled — it cannot race an
 * in-flight restore drain into recording a half-applied layout.
 *
 * Suspended in tmux work mode. The one viewport is not a grid: its zone 0 is
 * whichever session is shown, and every other session is in no zone at all,
 * so re-resolving against it would record the shown session at 0 and every
 * other at -1 — overwriting the grid zones the sessions return to on exit,
 * and the zones a restore after a runner restart re-seats them in. The ledger
 * keeps what was last WRITTEN, so the first pass after the mode returns to
 * grid re-resolves any session whose grid zone moved meanwhile.
 */
export function useRecordedZoneBackstop({
  workMode,
  assignments,
  tabs,
  pageId,
  recordedZones,
  restoreComplete,
}: RecordedZoneBackstopInput): void {
  const isTmux = workMode === "tmux";
  useEffect(() => {
    if (isTmux) return;
    const timer = setTimeout(() => {
      const observed = tabs
        .filter((tab): tab is typeof tab & { claudeSessionId: string } => !!tab.claudeSessionId)
        .map((tab) => ({
          claudeSessionId: tab.claudeSessionId,
          tabId: tab.id,
          zoneIndex: resolveZoneIndex(assignments, tab.id),
        }));
      // Before the page's restore has finished, only sessions this page has
      // already written are re-resolved: a first-seen session stays unseeded,
      // so it is first-seen again once `restoreComplete` flips.
      const { reemits, firstSeen } = planZoneReemits(
        recordedZones,
        restoreComplete
          ? observed
          : observed.filter((obs) => recordedZones.has(obs.claudeSessionId)),
      );
      for (const emit of reemits) {
        const tab = tabs.find((t) => t.id === emit.tabId);
        invoke("terminal_session_record_open", {
          ...buildSessionOpenArgs({
            assignments,
            tabs,
            tabId: emit.tabId,
            claudeSessionId: emit.claudeSessionId,
            configDir: tab?.claudeConfigDir,
            pageId,
          }),
        }).catch((err) => {
          logger.warn(
            `terminal_session_record_open (zone re-resolve) failed for ${emit.claudeSessionId}: ${err}`,
          );
        });
      }
      for (const obs of firstSeen) {
        invoke("terminal_session_sync_zone", buildSessionSyncZoneArgs(obs)).catch((err) => {
          logger.warn(
            `terminal_session_sync_zone (first-seen zone) failed for ${obs.claudeSessionId}: ${err}`,
          );
        });
      }
    }, ZONE_BACKSTOP_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [assignments, tabs, pageId, recordedZones, isTmux, restoreComplete]);
}

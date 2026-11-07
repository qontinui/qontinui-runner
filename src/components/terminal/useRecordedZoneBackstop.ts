import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";
import {
  buildSessionOpenArgs,
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

interface RecordedZoneBackstopInput {
  workMode: TerminalWorkMode;
  /** The page's live zone assignments. */
  assignments: ZoneAssignments;
  tabs: Array<OpenRecordTab & { claudeSessionId?: string; claudeConfigDir?: string }>;
  pageId: string;
  /** What each session's durable record was last WRITTEN with. */
  recordedZones: RecordedZoneLedger;
}

/**
 * Zone re-resolution backstop. Two jobs, one mechanism:
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
}: RecordedZoneBackstopInput): void {
  const isTmux = workMode === "tmux";
  useEffect(() => {
    if (isTmux) return;
    const timer = setTimeout(() => {
      const emits = planZoneReemits(
        recordedZones,
        tabs
          .filter((tab): tab is typeof tab & { claudeSessionId: string } => !!tab.claudeSessionId)
          .map((tab) => ({
            claudeSessionId: tab.claudeSessionId,
            tabId: tab.id,
            zoneIndex: resolveZoneIndex(assignments, tab.id),
          })),
      );
      for (const emit of emits) {
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
    }, ZONE_BACKSTOP_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [assignments, tabs, pageId, recordedZones, isTmux]);
}

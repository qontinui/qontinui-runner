/**
 * The zone-view facts the `maximize-zone` / `restore-zone` /
 * `toggle-maximize-zone` signatures predict from (plan
 * `2026-10-22-runner-ui-bridge-makes-an-agent-guess-its-own-vocabulary`,
 * Phase 2).
 *
 * A hook rather than inline reads in `TerminalPage` so the page gains one call
 * line, and so the inputs `ZoneGrid` renders headers from (layout, occupied
 * zones, focus, pins, compact view) are read from the same contexts it reads
 * them from. Plain values, memoized — no refs read during render.
 */

import { useMemo } from "react";

import { useTerminalSession, useUIStateCx } from "./contexts";
import { countLabelledZones, type ZoneViewFacts } from "./terminalPageSignatures";

export function useZoneViewFacts(): ZoneViewFacts {
  const { tabs, zoneLayout, labelsAndTags } = useTerminalSession();
  const { state: uiState } = useUIStateCx();
  const zoneCount = zoneLayout.layout.zones.length;
  const { maximizedZone, layoutId, assignments, focusedZone } = zoneLayout;
  const { pinnedZones } = labelsAndTags;
  const compactView = uiState.viewMode === "compact";

  return useMemo(
    () => ({
      maximizedZone,
      zoneCount,
      labelledZonesWhenTiled: countLabelledZones({
        layoutId,
        zoneCount,
        assignments,
        tabs,
        focusedZone,
        pinnedZones,
        compactView,
      }),
    }),
    [maximizedZone, zoneCount, layoutId, assignments, tabs, focusedZone, pinnedZones, compactView],
  );
}

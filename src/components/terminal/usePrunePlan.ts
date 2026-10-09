import { useMemo } from "react";

import { useTerminalSession } from "./contexts";
import { isPruneActionable, planPrune, type PrunePlan } from "./pruneTerminals";
import { useNow1Hz } from "./useNow1Hz";
import { resolveLayout } from "./useZoneLayout";

/**
 * The "Keep AI" plan for the current page, plus whether applying it would
 * change anything. Shared by `PruneTerminalsButton` (which renders and runs it)
 * and `StatusStrip` (which mounts the button only when this is actionable).
 *
 * `now` comes from the shared 1Hz tick, so a just-launched tab drops out of
 * the launch grace window (and into the close count) on its own.
 */
export function usePrunePlan(): PrunePlan & { targetZones: number; actionable: boolean } {
  const { tabs, zoneLayout, sessionStates } = useTerminalSession();
  const { assignments, layout } = zoneLayout;
  const zoneCount = layout.zones.length;
  const now = useNow1Hz();
  return useMemo(() => {
    const plan = planPrune(tabs, assignments, { sessionStates, now });
    const targetZones = resolveLayout(plan.layoutId, plan.keepIds.length).zones.length;
    const actionable = isPruneActionable(plan, zoneCount, targetZones, assignments);
    return { ...plan, targetZones, actionable };
  }, [tabs, assignments, zoneCount, sessionStates, now]);
}

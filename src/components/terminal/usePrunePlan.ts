import { useMemo } from "react";

import { useKnownClaudePanes } from "./claudePresence";
import { useTerminalSession } from "./contexts";
import { isPruneActionable, planPrune, type PrunePlan, type PruneSignals } from "./pruneTerminals";
import { useNow1Hz } from "./useNow1Hz";
import { resolveLayout } from "./useZoneLayout";

/**
 * The "Keep AI" plan for the current page, plus whether applying it would
 * change anything. Shared by `PruneTerminalsButton` (which renders and runs it)
 * and `StatusStrip` (which mounts the button only when this is actionable).
 *
 * `now` comes from the shared 1Hz tick, so a just-launched tab drops out of
 * the launch grace window (and into the close count) on its own.
 *
 * Panes an earlier probe read `live` are kept on every read, so once the
 * button has rescued a pane both callers stop counting it as a candidate.
 * `claudePresence` is the button's armed probe; without it the plan is the
 * estimate that decides whether to offer the button at all.
 */
export function usePrunePlan(
  claudePresence?: PruneSignals["claudePresence"],
): PrunePlan & { targetZones: number; actionable: boolean } {
  const { tabs, zoneLayout, sessionStates } = useTerminalSession();
  const { assignments, layout } = zoneLayout;
  const zoneCount = layout.zones.length;
  const now = useNow1Hz();
  const knownClaudePanes = useKnownClaudePanes();
  return useMemo(() => {
    const plan = planPrune(tabs, assignments, {
      sessionStates,
      now,
      knownClaudePanes,
      claudePresence,
    });
    const targetZones = resolveLayout(plan.layoutId, plan.keepIds.length).zones.length;
    const actionable = isPruneActionable(plan, zoneCount, targetZones, assignments);
    return { ...plan, targetZones, actionable };
  }, [tabs, assignments, zoneCount, sessionStates, now, knownClaudePanes, claudePresence]);
}

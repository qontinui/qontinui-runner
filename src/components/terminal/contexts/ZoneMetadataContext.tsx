/**
 * ZoneMetadataContext
 *
 * Owns zone labels, tags, pins, event history, focus history, and metrics.
 * Reads from TerminalSessionContext for layout and zone assignment data
 * (the collapsed-into-one context per Phase 4 plan).
 */

import { createContext, useMemo, type ReactNode } from "react";
import { useTerminalSession } from "./TerminalSessionContext";
import type { useZoneLabelsAndTags } from "../useZoneLabelsAndTags";
import { useEventHistory } from "../useEventHistory";
import { useFocusHistory } from "../useFocusHistory";

type LabelsAndTagsReturn = ReturnType<typeof useZoneLabelsAndTags>;
type EventHistoryReturn = ReturnType<typeof useEventHistory>;
type FocusHistoryReturn = ReturnType<typeof useFocusHistory>;

export interface ZoneMetadataContextValue {
  labelsAndTags: LabelsAndTagsReturn;
  eventHistory: EventHistoryReturn["eventHistory"];
  addHistoryEvent: EventHistoryReturn["addHistoryEvent"];
  metrics: EventHistoryReturn["metrics"];
  incrementMetric: EventHistoryReturn["incrementMetric"];
  focusHistory: FocusHistoryReturn;
}

export const ZoneMetadataContext = createContext<ZoneMetadataContextValue | null>(null);

interface ZoneMetadataProviderProps {
  children: ReactNode;
}

export function ZoneMetadataProvider({ children }: ZoneMetadataProviderProps) {
  const { zoneLayout, labelsAndTags } = useTerminalSession();

  // `labelsAndTags` now lives in the per-page `PageSessionScope` (so each page
  // owns its own page-namespaced labels and the cold-start init backstop can
  // apply restored cosmetics to the right page). Re-expose the exact reference
  // here so every downstream `useZoneMetadata()` consumer is unchanged.
  const { eventHistory, addHistoryEvent, metrics, incrementMetric } = useEventHistory();

  const focusHistory = useFocusHistory(zoneLayout.focusedZone, zoneLayout.setFocusedZone);

  const value = useMemo<ZoneMetadataContextValue>(
    () => ({
      labelsAndTags,
      eventHistory,
      addHistoryEvent,
      metrics,
      incrementMetric,
      focusHistory,
    }),
    [labelsAndTags, eventHistory, addHistoryEvent, metrics, incrementMetric, focusHistory],
  );

  return <ZoneMetadataContext.Provider value={value}>{children}</ZoneMetadataContext.Provider>;
}

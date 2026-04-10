/**
 * ZoneMetadataContext
 *
 * Owns zone labels, tags, pins, event history, focus history, and metrics.
 * Reads from TerminalCoreContext for layout and zone assignment data.
 */

import { createContext, useMemo, type ReactNode } from "react";
import { useTerminalCore } from "./useTerminalCore";
import { useZoneLabelsAndTags } from "../useZoneLabelsAndTags";
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
  const { pageId, zoneLayout } = useTerminalCore();

  const labelsAndTags = useZoneLabelsAndTags(
    zoneLayout.layoutId,
    zoneLayout.assignments,
    pageId,
  );

  const {
    eventHistory,
    addHistoryEvent,
    metrics,
    incrementMetric,
  } = useEventHistory();

  const focusHistory = useFocusHistory(
    zoneLayout.focusedZone,
    zoneLayout.setFocusedZone,
  );

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

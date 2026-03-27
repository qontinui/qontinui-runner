import { useEffect, useMemo, useRef, useState } from "react";

export function useUnreadTracking(
  focusedZone: number,
  assignments: Record<number, string>,
  lastOutputLines: Record<string, string[]>,
): {
  unreadZones: Set<string>;
} {
  // Track last-seen line count per tab for unread indicators
  const lastSeenLineCountRef = useRef<Record<string, number>>({});
  // State-based snapshot so we can derive unreadZones without reading ref.current during render
  const [lastSeenSnapshot, setLastSeenSnapshot] = useState<Record<string, number>>({});

  // Update last-seen line count when zone gains focus (clears unread indicator)
  useEffect(() => {
    const tabId = assignments[focusedZone];
    if (tabId) {
      const count = (lastOutputLines[tabId] ?? []).length;
      lastSeenLineCountRef.current[tabId] = count;
      // eslint-disable-next-line react-hooks/set-state-in-effect -- sync snapshot with ref on focus change
      setLastSeenSnapshot((prev) => ({ ...prev, [tabId]: count }));
    }
  }, [focusedZone, assignments, lastOutputLines]);

  // Compute which zones have unread output using state snapshot (not ref.current)
  const unreadZones = useMemo(() => {
    const unread = new Set<string>();
    for (const [zoneStr, tabId] of Object.entries(assignments)) {
      const currentCount = (lastOutputLines[tabId] ?? []).length;
      const lastSeen = lastSeenSnapshot[tabId] ?? 0;
      if (currentCount > lastSeen && Number(zoneStr) !== focusedZone) {
        unread.add(tabId);
      }
    }
    return unread;
  }, [lastOutputLines, assignments, focusedZone, lastSeenSnapshot]);

  return { unreadZones };
}

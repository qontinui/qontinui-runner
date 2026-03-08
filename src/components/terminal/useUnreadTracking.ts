import { useEffect, useMemo, useRef } from "react";

export function useUnreadTracking(
  focusedZone: number,
  assignments: Record<number, string>,
  lastOutputLines: Record<string, string[]>,
): {
  unreadZones: Set<string>;
} {
  // Track last-seen line count per tab for unread indicators
  const lastSeenLineCountRef = useRef<Record<string, number>>({});

  // Update last-seen line count when zone gains focus (clears unread indicator)
  useEffect(() => {
    const tabId = assignments[focusedZone];
    if (tabId) {
      lastSeenLineCountRef.current[tabId] = (lastOutputLines[tabId] ?? []).length;
    }
  }, [focusedZone, assignments, lastOutputLines]);

  // Compute which zones have unread output
  const unreadZones = useMemo(() => {
    const unread = new Set<string>();
    for (const [zoneStr, tabId] of Object.entries(assignments)) {
      const currentCount = (lastOutputLines[tabId] ?? []).length;
      const lastSeen = lastSeenLineCountRef.current[tabId] ?? 0;
      if (currentCount > lastSeen && Number(zoneStr) !== focusedZone) {
        unread.add(tabId);
      }
    }
    return unread;
  }, [lastOutputLines, assignments, focusedZone]);

  return { unreadZones };
}

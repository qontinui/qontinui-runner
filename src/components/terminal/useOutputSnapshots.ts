import { useCallback, useMemo, useRef, useState } from "react";
import { getTerminalHotStore } from "./terminalHotStore";

/**
 * Zone output snapshot / diff state.
 *
 * Reads `lastOutputLines` LAZILY out of the page's hot store at click time
 * rather than taking the whole map as a parameter. That keeps every returned
 * callback identity-stable, so this hook no longer re-publishes the terminal
 * session context value on each output frame (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 1 / §0 A1).
 */
export function useOutputSnapshots(pageId: string): {
  snapshotZone: (tabId: string) => void;
  compareSnapshot: (tabId: string) => void;
  snapshotZones: Set<string>;
  snapshotDiff: { tabId: string; snapshot: string[]; current: string[] } | null;
  clearSnapshotDiff: () => void;
  diffZones: [number, number] | null;
  setDiffZones: (zones: [number, number] | null) => void;
} {
  const store = getTerminalHotStore(pageId);
  const [outputSnapshots, setOutputSnapshots] = useState<Record<string, string[]>>({});
  const [snapshotDiff, setSnapshotDiff] = useState<{
    tabId: string;
    snapshot: string[];
    current: string[];
  } | null>(null);
  const [diffZones, setDiffZones] = useState<[number, number] | null>(null);
  // Mirror of `outputSnapshots` so `compareSnapshot` can read the current
  // snapshots without taking them as a dependency (which would churn its
  // identity on every capture).
  const snapshotsRef = useRef<Record<string, string[]>>(outputSnapshots);

  // Set of tab IDs that have output snapshots stored
  const snapshotZones = useMemo(() => new Set(Object.keys(outputSnapshots)), [outputSnapshots]);

  const snapshotZone = useCallback(
    (tabId: string) => {
      const next = {
        ...snapshotsRef.current,
        [tabId]: [...store.getLastOutputLines(tabId)],
      };
      snapshotsRef.current = next;
      setOutputSnapshots(next);
    },
    [store],
  );

  const compareSnapshot = useCallback(
    (tabId: string) => {
      const snapshot = snapshotsRef.current[tabId];
      if (!snapshot) return;
      setSnapshotDiff({
        tabId,
        snapshot,
        current: store.getLastOutputLines(tabId),
      });
    },
    [store],
  );

  const clearSnapshotDiff = useCallback(() => {
    setSnapshotDiff(null);
  }, []);

  return useMemo(
    () => ({
      snapshotZone,
      compareSnapshot,
      snapshotZones,
      snapshotDiff,
      clearSnapshotDiff,
      diffZones,
      setDiffZones,
    }),
    [snapshotZone, compareSnapshot, snapshotZones, snapshotDiff, clearSnapshotDiff, diffZones],
  );
}

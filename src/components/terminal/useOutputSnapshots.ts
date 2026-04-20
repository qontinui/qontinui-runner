import { useCallback, useMemo, useRef, useState } from "react";

export function useOutputSnapshots(lastOutputLines: Record<string, string[]>): {
  snapshotZone: (tabId: string) => void;
  compareSnapshot: (tabId: string) => void;
  snapshotZones: Set<string>;
  snapshotDiff: { tabId: string; snapshot: string[]; current: string[] } | null;
  clearSnapshotDiff: () => void;
  diffZones: [number, number] | null;
  setDiffZones: (zones: [number, number] | null) => void;
} {
  const outputSnapshotsRef = useRef<Record<string, string[]>>({});
  const [snapshotDiff, setSnapshotDiff] = useState<{
    tabId: string;
    snapshot: string[];
    current: string[];
  } | null>(null);
  const [snapshotCounter, setSnapshotCounter] = useState(0);
  const [diffZones, setDiffZones] = useState<[number, number] | null>(null);

  // Set of tab IDs that have output snapshots stored
  const snapshotZones = useMemo(
    // eslint-disable-next-line react-hooks/refs
    () => new Set(Object.keys(outputSnapshotsRef.current)),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [snapshotCounter],
  );

  const snapshotZone = useCallback(
    (tabId: string) => {
      outputSnapshotsRef.current[tabId] = [...(lastOutputLines[tabId] ?? [])];
      setSnapshotCounter((c) => c + 1);
    },
    [lastOutputLines],
  );

  const compareSnapshot = useCallback(
    (tabId: string) => {
      const snapshot = outputSnapshotsRef.current[tabId];
      if (snapshot) {
        setSnapshotDiff({
          tabId,
          snapshot,
          current: lastOutputLines[tabId] ?? [],
        });
      }
    },
    [lastOutputLines],
  );

  const clearSnapshotDiff = useCallback(() => {
    setSnapshotDiff(null);
  }, []);

  return {
    snapshotZone,
    compareSnapshot,
    snapshotZones,
    snapshotDiff,
    clearSnapshotDiff,
    diffZones,
    setDiffZones,
  };
}

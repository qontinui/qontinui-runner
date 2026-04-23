import { useCallback, useMemo, useState } from "react";

export function useOutputSnapshots(lastOutputLines: Record<string, string[]>): {
  snapshotZone: (tabId: string) => void;
  compareSnapshot: (tabId: string) => void;
  snapshotZones: Set<string>;
  snapshotDiff: { tabId: string; snapshot: string[]; current: string[] } | null;
  clearSnapshotDiff: () => void;
  diffZones: [number, number] | null;
  setDiffZones: (zones: [number, number] | null) => void;
} {
  const [outputSnapshots, setOutputSnapshots] = useState<Record<string, string[]>>({});
  const [snapshotDiff, setSnapshotDiff] = useState<{
    tabId: string;
    snapshot: string[];
    current: string[];
  } | null>(null);
  const [diffZones, setDiffZones] = useState<[number, number] | null>(null);

  // Set of tab IDs that have output snapshots stored
  const snapshotZones = useMemo(() => new Set(Object.keys(outputSnapshots)), [outputSnapshots]);

  const snapshotZone = useCallback(
    (tabId: string) => {
      setOutputSnapshots((prev) => ({
        ...prev,
        [tabId]: [...(lastOutputLines[tabId] ?? [])],
      }));
    },
    [lastOutputLines],
  );

  const compareSnapshot = useCallback(
    (tabId: string) => {
      const snapshot = outputSnapshots[tabId];
      if (snapshot) {
        setSnapshotDiff({
          tabId,
          snapshot,
          current: lastOutputLines[tabId] ?? [],
        });
      }
    },
    [outputSnapshots, lastOutputLines],
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

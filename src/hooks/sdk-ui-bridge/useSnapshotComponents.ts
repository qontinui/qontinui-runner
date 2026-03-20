/**
 * useSnapshotComponents — Snapshot and component management sub-hook.
 */

import { useState, useCallback } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

export interface UseSnapshotComponentsReturn {
  snapshot: unknown | null;
  components: unknown[];
  refreshSnapshot: () => Promise<void>;
  refreshComponents: () => Promise<void>;
  setSnapshot: React.Dispatch<React.SetStateAction<unknown | null>>;
  setComponents: React.Dispatch<React.SetStateAction<unknown[]>>;
}

export function useSnapshotComponents(): UseSnapshotComponentsReturn {
  const [snapshot, setSnapshot] = useState<unknown | null>(null);
  const [components, setComponents] = useState<unknown[]>([]);

  const refreshSnapshot = useCallback(async () => {
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/snapshot`);
      const json = await resp.json();
      if (json.success !== false) {
        setSnapshot(json.data || json);
      }
    } catch {
      // Non-fatal
    }
  }, []);

  const refreshComponents = useCallback(async () => {
    try {
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/components`);
      const json = await resp.json();
      if (json.success !== false) {
        setComponents(json.data?.components || json.components || json.data || []);
      }
    } catch {
      // Non-fatal
    }
  }, []);

  return {
    snapshot,
    components,
    refreshSnapshot,
    refreshComponents,
    setSnapshot,
    setComponents,
  };
}

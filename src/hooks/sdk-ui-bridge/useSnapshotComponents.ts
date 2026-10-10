/**
 * useSnapshotComponents — Snapshot and component management sub-hook.
 */

import { useState, useCallback } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

export interface UseSnapshotComponentsReturn {
  snapshot: unknown | null;
  components: unknown[];
  refreshSnapshot: (options?: { recency?: string }) => Promise<void>;
  refreshComponents: (options?: { recency?: string }) => Promise<void>;
  setSnapshot: React.Dispatch<React.SetStateAction<unknown | null>>;
  setComponents: React.Dispatch<React.SetStateAction<unknown[]>>;
}

export function useSnapshotComponents(): UseSnapshotComponentsReturn {
  const [snapshot, setSnapshot] = useState<unknown | null>(null);
  const [components, setComponents] = useState<unknown[]>([]);

  const refreshSnapshot = useCallback(async (options?: { recency?: string }) => {
    try {
      const recency = options?.recency ?? "any";
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/snapshot?recency=${recency}`);
      const json = await resp.json();
      if (json.success !== false) {
        setSnapshot(json.data || json);
      }
    } catch {
      // Non-fatal
    }
  }, []);

  const refreshComponents = useCallback(async (options?: { recency?: string }) => {
    try {
      const recency = options?.recency ?? "any";
      const resp = await tracedFetch(`${getApiBase()}/ui-bridge/sdk/components?recency=${recency}`);
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

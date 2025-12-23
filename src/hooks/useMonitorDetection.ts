/**
 * useMonitorDetection
 *
 * Hook for detecting and managing system monitors.
 * Responsibility: Detect available monitors and track selected monitors (supports multi-select).
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "../types/displayProfile";

interface UseMonitorDetectionOptions {
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  detectOnMount?: boolean;
}

interface UseMonitorDetectionReturn {
  /** Array of selected monitor indices */
  selectedMonitors: number[];
  /** Set selected monitors (array for multi-select) */
  setSelectedMonitors: (indices: number[]) => void;
  /** Select monitors and save to persistence */
  selectMonitorsWithPersistence: (indices: number[]) => Promise<void>;
  /** Available monitor indices */
  availableMonitors: number[];
  /** Detect system monitors */
  detectSystemMonitors: () => Promise<void>;

  // Legacy single-monitor API for backward compatibility
  /** @deprecated Use selectedMonitors[0] instead */
  selectedMonitor: number;
  /** @deprecated Use setSelectedMonitors instead */
  setSelectedMonitor: (index: number) => void;
  /** @deprecated Use selectMonitorsWithPersistence instead */
  selectMonitorWithPersistence: (index: number) => Promise<void>;
}

/**
 * Hook to manage monitor detection and selection (supports multi-select)
 */
export function useMonitorDetection(
  options: UseMonitorDetectionOptions = {},
): UseMonitorDetectionReturn {
  const { onLog, detectOnMount = true } = options;

  const [selectedMonitors, setSelectedMonitors] = useState<number[]>([0]);
  const [availableMonitors, setAvailableMonitors] = useState<number[]>([0]);

  /**
   * Select monitors and save as last used
   */
  const selectMonitorsWithPersistence = useCallback(async (indices: number[]) => {
    // Ensure at least one monitor is selected
    const validIndices = indices.length > 0 ? indices : [0];
    setSelectedMonitors(validIndices);

    // Save the monitor indices as the last used monitors
    try {
      await invoke("save_last_monitor_indices", { monitorIndices: validIndices });
      console.log("[MONITOR_DETECTION] Saved last monitor indices:", validIndices);
    } catch (saveError) {
      // Fallback to old single-monitor API for backward compatibility
      try {
        await invoke("save_last_monitor_index", { monitorIndex: validIndices[0] });
        console.log("[MONITOR_DETECTION] Saved last monitor index (legacy):", validIndices[0]);
        // eslint-disable-next-line @typescript-eslint/no-unused-vars
      } catch (_legacyError) {
        console.error("[MONITOR_DETECTION] Failed to save last monitor indices:", saveError);
      }
    }
  }, []);

  /**
   * Detect system monitors
   */
  const detectSystemMonitors = useCallback(async () => {
    try {
      const result =
        await invoke<CommandResponse<{ indices?: number[]; count?: number }>>("get_monitors");
      if (result.success && result.data) {
        const indices = result.data.indices || [0];
        setAvailableMonitors(indices);
        onLog?.("info", `Detected ${result.data.count} monitor(s)`);

        // Validate currently selected monitors
        setSelectedMonitors((current) => {
          const validSelections = current.filter((idx) => indices.includes(idx));
          // If no valid selections, reset to primary monitor
          return validSelections.length > 0 ? validSelections : [0];
        });
      }
    } catch (error) {
      console.error("[MONITOR_DETECTION] Failed to detect monitors:", error);
      onLog?.("warning", "Could not detect monitors, using default");
      setAvailableMonitors([0]);
    }
  }, [onLog]);

  /**
   * Detect monitors on mount
   */
  useEffect(() => {
    if (detectOnMount) {
      console.log("[MONITOR_DETECTION] Detecting monitors on mount");
      detectSystemMonitors();
    }
  }, [detectOnMount, detectSystemMonitors]);

  // Legacy single-monitor API for backward compatibility
  const selectedMonitor = selectedMonitors[0] ?? 0;

  const setSelectedMonitor = useCallback((index: number) => {
    setSelectedMonitors([index]);
  }, []);

  const selectMonitorWithPersistence = useCallback(
    async (index: number) => {
      await selectMonitorsWithPersistence([index]);
    },
    [selectMonitorsWithPersistence],
  );

  return {
    // New multi-monitor API
    selectedMonitors,
    setSelectedMonitors,
    selectMonitorsWithPersistence,
    availableMonitors,
    detectSystemMonitors,

    // Legacy single-monitor API
    selectedMonitor,
    setSelectedMonitor,
    selectMonitorWithPersistence,
  };
}

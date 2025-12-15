/**
 * useMonitorDetection
 *
 * Hook for detecting and managing system monitors.
 * Responsibility: Detect available monitors and track selected monitor.
 */

import { useState, useCallback, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

interface UseMonitorDetectionOptions {
  onLog?: (level: "info" | "warning" | "error" | "debug" | "success", message: string) => void;
  detectOnMount?: boolean;
}

interface UseMonitorDetectionReturn {
  selectedMonitor: number;
  setSelectedMonitor: (index: number) => void;
  selectMonitorWithPersistence: (index: number) => Promise<void>;
  availableMonitors: number[];
  detectSystemMonitors: () => Promise<void>;
}

/**
 * Hook to manage monitor detection and selection
 */
export function useMonitorDetection(
  options: UseMonitorDetectionOptions = {},
): UseMonitorDetectionReturn {
  const { onLog, detectOnMount = true } = options;

  const [selectedMonitor, setSelectedMonitor] = useState(0);
  const [availableMonitors, setAvailableMonitors] = useState<number[]>([0]);

  /**
   * Select a monitor and save it as the last used
   */
  const selectMonitorWithPersistence = useCallback(async (index: number) => {
    setSelectedMonitor(index);

    // Save the monitor index as the last used monitor
    try {
      await invoke("save_last_monitor_index", { monitorIndex: index });
      console.log("[MONITOR_DETECTION] Saved last monitor index:", index);
    } catch (saveError) {
      console.error("[MONITOR_DETECTION] Failed to save last monitor index:", saveError);
      // Non-critical error, don't throw
    }
  }, []);

  /**
   * Detect system monitors
   */
  const detectSystemMonitors = useCallback(async () => {
    try {
      const result: any = await invoke("get_monitors");
      if (result.success && result.data) {
        const indices = result.data.indices || [0];
        setAvailableMonitors(indices);
        onLog?.("info", `Detected ${result.data.count} monitor(s)`);

        // If currently selected monitor is not available, reset to primary
        setSelectedMonitor((current) => {
          if (!indices.includes(current)) {
            return 0;
          }
          return current;
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

  return {
    selectedMonitor,
    setSelectedMonitor,
    selectMonitorWithPersistence,
    availableMonitors,
    detectSystemMonitors,
  };
}

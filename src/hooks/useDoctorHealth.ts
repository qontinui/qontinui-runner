/**
 * useDoctorHealth Hook
 *
 * React hook for monitoring Doctor health events.
 * Listens to doctor-warning, doctor-stuck, and doctor-healthy Tauri events
 * to track the health state of monitored AI processes.
 */

import { useState, useEffect, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";

// ============================================================================
// Types
// ============================================================================

export type DoctorHealthStatus = "healthy" | "warning" | "stuck";

export interface DoctorProcessState {
  pid: number;
  processLabel: string;
  status: DoctorHealthStatus;
  message?: string;
  inactiveChecks?: number;
  lastUpdated: number;
}

interface DoctorWarningPayload {
  type: "Warning";
  pid: number;
  process_label: string;
  message: string;
  inactive_checks: number;
}

interface DoctorStuckPayload {
  type: "Stuck";
  pid: number;
  process_label: string;
  message: string;
  inactive_checks: number;
}

interface DoctorHealthyPayload {
  type: "Healthy";
  pid: number;
  process_label: string;
}

// ============================================================================
// Main Hook
// ============================================================================

interface UseDoctorHealthReturn {
  /** Map of PID to process health state */
  processes: Map<number, DoctorProcessState>;
  /** Number of processes with warning or stuck status */
  unhealthyCount: number;
  /** Number of stuck processes */
  stuckCount: number;
  /** Whether any process is stuck */
  hasStuckProcess: boolean;
  /** Worst health status across all processes */
  worstStatus: DoctorHealthStatus | null;
  /** Dismiss/clear a process from tracking (e.g., after user acknowledges) */
  dismiss: (pid: number) => void;
}

export function useDoctorHealth(): UseDoctorHealthReturn {
  const [processes, setProcesses] = useState<Map<number, DoctorProcessState>>(new Map());

  useEffect(() => {
    const unlistenWarning = listen<DoctorWarningPayload>("doctor-warning", (event) => {
      const { pid, process_label, message, inactive_checks } = event.payload;
      setProcesses((prev) => {
        const next = new Map(prev);
        next.set(pid, {
          pid,
          processLabel: process_label,
          status: "warning",
          message,
          inactiveChecks: inactive_checks,
          lastUpdated: Date.now(),
        });
        return next;
      });
    });

    const unlistenStuck = listen<DoctorStuckPayload>("doctor-stuck", (event) => {
      const { pid, process_label, message, inactive_checks } = event.payload;
      setProcesses((prev) => {
        const next = new Map(prev);
        next.set(pid, {
          pid,
          processLabel: process_label,
          status: "stuck",
          message,
          inactiveChecks: inactive_checks,
          lastUpdated: Date.now(),
        });
        return next;
      });
    });

    const unlistenHealthy = listen<DoctorHealthyPayload>("doctor-healthy", (event) => {
      const { pid } = event.payload;
      setProcesses((prev) => {
        const next = new Map(prev);
        // Remove recovered processes from tracking
        next.delete(pid);
        return next;
      });
    });

    return () => {
      unlistenWarning.then((fn) => fn());
      unlistenStuck.then((fn) => fn());
      unlistenHealthy.then((fn) => fn());
    };
  }, []);

  const dismiss = useCallback((pid: number) => {
    setProcesses((prev) => {
      const next = new Map(prev);
      next.delete(pid);
      return next;
    });
  }, []);

  // Computed values
  const unhealthyCount = processes.size;
  const stuckCount = Array.from(processes.values()).filter((p) => p.status === "stuck").length;
  const hasStuckProcess = stuckCount > 0;

  let worstStatus: DoctorHealthStatus | null = null;
  if (stuckCount > 0) {
    worstStatus = "stuck";
  } else if (unhealthyCount > 0) {
    worstStatus = "warning";
  }

  return {
    processes,
    unhealthyCount,
    stuckCount,
    hasStuckProcess,
    worstStatus,
    dismiss,
  };
}

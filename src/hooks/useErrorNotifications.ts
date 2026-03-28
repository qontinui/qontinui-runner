/**
 * useErrorNotifications — Shows toast notifications for new critical/error events.
 * Mount once at App level. Pass showToast from useToast().
 */

import { useEffect, useRef, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { errorMonitorService } from "../services/error-monitor-service";
import type { ShowToastFn } from "./useToast";

const MAX_TOASTS_PER_BATCH = 3;

export function useErrorNotifications(showToast: ShowToastFn) {
  const notifiedIds = useRef(new Set<number>());

  const handleNewErrors = useCallback(async () => {
    try {
      // Fetch errors from last ~1 minute
      const recent = await errorMonitorService.getRecentErrors(0.017, 10);

      let shown = 0;
      for (const err of recent) {
        if (shown >= MAX_TOASTS_PER_BATCH) break;
        if (notifiedIds.current.has(err.id)) continue;
        if (err.severity !== "critical" && err.severity !== "error") continue;

        notifiedIds.current.add(err.id);
        const prefix = err.severity === "critical" ? "CRITICAL" : "Error";
        const msg =
          err.message.length > 120
            ? err.message.slice(0, 120) + "\u2026"
            : err.message;
        showToast(`[${prefix}] ${msg}`, "error");
        shown++;
      }

      // Keep set from growing unbounded — prune entries older than 1000
      if (notifiedIds.current.size > 1000) {
        const arr = Array.from(notifiedIds.current);
        notifiedIds.current = new Set(arr.slice(-500));
      }
    } catch {
      // Silently ignore — error notifications are best-effort
    }
  }, [showToast]);

  useEffect(() => {
    const unlisten = listen("error-event-detected", handleNewErrors);
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [handleNewErrors]);
}

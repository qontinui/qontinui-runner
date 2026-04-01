/**
 * useErrorNotifications — Shows toast notifications for new critical/error events.
 * Mount once at App level. Pass showToast from useToast().
 *
 * Respects user notification preferences from instanceStorage:
 * - severityFilter: minimum severity to toast (critical | error | warning | off)
 * - mutedSources: log source names to suppress
 * - maxPerBatch: max toasts per event batch
 */

import { useEffect, useRef, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { errorMonitorService } from "../services/error-monitor-service";
import { instanceStorage } from "@/lib/instance-storage";
import { severityPassesFilter } from "./useNotificationPreferences";
import type {
  NotificationPreferences,
  NotificationSeverityFilter,
} from "./useNotificationPreferences";
import type { ShowToastFn } from "./useToast";

const PREFS_STORAGE_KEY = "qontinui-notification-prefs";
const TOAST_RATE_LIMIT_MS = 2000;

/** Read preferences directly from storage each batch (always fresh). */
function readPrefs(): NotificationPreferences {
  const raw = instanceStorage.getJSON<Partial<NotificationPreferences> | null>(
    PREFS_STORAGE_KEY,
    null,
  );
  const isValidSeverity = (v: unknown): v is NotificationSeverityFilter =>
    v === "critical" || v === "error" || v === "warning" || v === "off";

  if (!raw) {
    return { severityFilter: "error", mutedSources: [], maxPerBatch: 3 };
  }
  return {
    severityFilter: isValidSeverity(raw.severityFilter) ? raw.severityFilter : "error",
    mutedSources: Array.isArray(raw.mutedSources) ? raw.mutedSources : [],
    maxPerBatch: typeof raw.maxPerBatch === "number" && raw.maxPerBatch >= 0 ? raw.maxPerBatch : 3,
  };
}

export function useErrorNotifications(showToast: ShowToastFn) {
  const notifiedIds = useRef(new Set<number>());
  const lastToastBatchTime = useRef(0);

  const handleNewErrors = useCallback(async () => {
    try {
      const prefs = readPrefs();

      // If notifications are disabled, skip entirely
      if (prefs.severityFilter === "off" || prefs.maxPerBatch === 0) return;

      // Fetch errors from last ~1 minute
      const recent = await errorMonitorService.getRecentErrors(0.017, 10);

      const now = Date.now();
      const timeSinceLastBatch = now - lastToastBatchTime.current;

      // Count how many new errors we would show (applying user preferences)
      const newErrors = recent.filter(
        (err) =>
          !notifiedIds.current.has(err.id) &&
          severityPassesFilter(err.severity, prefs.severityFilter) &&
          !(err.logSourceName && prefs.mutedSources.includes(err.logSourceName)),
      );

      if (newErrors.length === 0) return;

      // Mark all as notified regardless of rate limiting
      for (const err of newErrors) {
        notifiedIds.current.add(err.id);
      }

      // Rate limit: if too soon since last batch, show a summary toast instead
      if (timeSinceLastBatch < TOAST_RATE_LIMIT_MS) {
        showToast(
          `${newErrors.length} more error(s) detected — check Error Monitor for details`,
          "error",
        );
        return;
      }

      lastToastBatchTime.current = now;

      let shown = 0;
      for (const err of newErrors) {
        if (shown >= prefs.maxPerBatch) break;
        const prefix = err.severity === "critical" ? "CRITICAL" : "Error";
        const msg = err.message.length > 120 ? err.message.slice(0, 120) + "\u2026" : err.message;
        showToast(`[${prefix}] ${msg}`, "error");
        shown++;
      }

      if (newErrors.length > prefs.maxPerBatch) {
        showToast(`${newErrors.length - prefs.maxPerBatch} more error(s) detected`, "error");
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

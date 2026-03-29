/**
 * useNotificationPreferences — User-configurable error notification preferences.
 *
 * Persisted in instanceStorage (localStorage). Consumed by useErrorNotifications
 * to filter which errors produce toasts.
 */

import { useState, useCallback, useMemo } from "react";
import { instanceStorage } from "@/lib/instance-storage";

const STORAGE_KEY = "qontinui-notification-prefs";

/** Minimum severity level that triggers a toast. "off" disables all toasts. */
export type NotificationSeverityFilter = "critical" | "error" | "warning" | "off";

export interface NotificationPreferences {
  /** Minimum severity to show toasts. Default: "error" (shows critical + error). */
  severityFilter: NotificationSeverityFilter;
  /** Log source names whose errors are suppressed from toasts. */
  mutedSources: string[];
  /** Maximum toasts shown per event batch. Default: 3. */
  maxPerBatch: number;
}

const DEFAULTS: NotificationPreferences = {
  severityFilter: "error",
  mutedSources: [],
  maxPerBatch: 3,
};

function readPrefs(): NotificationPreferences {
  const raw = instanceStorage.getJSON<Partial<NotificationPreferences> | null>(
    STORAGE_KEY,
    null,
  );
  if (!raw) return { ...DEFAULTS };
  return {
    severityFilter: isValidSeverity(raw.severityFilter) ? raw.severityFilter : DEFAULTS.severityFilter,
    mutedSources: Array.isArray(raw.mutedSources) ? raw.mutedSources : DEFAULTS.mutedSources,
    maxPerBatch:
      typeof raw.maxPerBatch === "number" && raw.maxPerBatch >= 0
        ? raw.maxPerBatch
        : DEFAULTS.maxPerBatch,
  };
}

function writePrefs(prefs: NotificationPreferences): void {
  instanceStorage.setJSON(STORAGE_KEY, prefs);
}

function isValidSeverity(v: unknown): v is NotificationSeverityFilter {
  return v === "critical" || v === "error" || v === "warning" || v === "off";
}

/** Severity weight for comparisons. Lower = more severe. */
const SEVERITY_RANK: Record<string, number> = {
  critical: 0,
  error: 1,
  warning: 2,
  info: 3,
  debug: 4,
};

/**
 * Check whether a given event severity passes the user's filter threshold.
 * E.g., if filter is "warning", then critical, error, and warning all pass.
 */
export function severityPassesFilter(
  eventSeverity: string,
  filterLevel: NotificationSeverityFilter,
): boolean {
  if (filterLevel === "off") return false;
  const eventRank = SEVERITY_RANK[eventSeverity] ?? 99;
  const filterRank = SEVERITY_RANK[filterLevel] ?? 1;
  return eventRank <= filterRank;
}

export interface UseNotificationPreferencesResult {
  prefs: NotificationPreferences;
  setSeverityFilter: (filter: NotificationSeverityFilter) => void;
  setMaxPerBatch: (max: number) => void;
  muteSource: (sourceName: string) => void;
  unmuteSource: (sourceName: string) => void;
  isSourceMuted: (sourceName: string) => boolean;
  resetToDefaults: () => void;
}

export function useNotificationPreferences(): UseNotificationPreferencesResult {
  const [prefs, setPrefs] = useState<NotificationPreferences>(readPrefs);

  const update = useCallback((updater: (prev: NotificationPreferences) => NotificationPreferences) => {
    setPrefs((prev) => {
      const next = updater(prev);
      writePrefs(next);
      return next;
    });
  }, []);

  const setSeverityFilter = useCallback(
    (filter: NotificationSeverityFilter) => {
      update((prev) => ({ ...prev, severityFilter: filter }));
    },
    [update],
  );

  const setMaxPerBatch = useCallback(
    (max: number) => {
      update((prev) => ({ ...prev, maxPerBatch: Math.max(0, Math.round(max)) }));
    },
    [update],
  );

  const muteSource = useCallback(
    (sourceName: string) => {
      update((prev) => ({
        ...prev,
        mutedSources: prev.mutedSources.includes(sourceName)
          ? prev.mutedSources
          : [...prev.mutedSources, sourceName],
      }));
    },
    [update],
  );

  const unmuteSource = useCallback(
    (sourceName: string) => {
      update((prev) => ({
        ...prev,
        mutedSources: prev.mutedSources.filter((s) => s !== sourceName),
      }));
    },
    [update],
  );

  const isSourceMuted = useCallback(
    (sourceName: string) => prefs.mutedSources.includes(sourceName),
    [prefs.mutedSources],
  );

  const resetToDefaults = useCallback(() => {
    update(() => ({ ...DEFAULTS }));
  }, [update]);

  return useMemo(
    () => ({
      prefs,
      setSeverityFilter,
      setMaxPerBatch,
      muteSource,
      unmuteSource,
      isSourceMuted,
      resetToDefaults,
    }),
    [prefs, setSeverityFilter, setMaxPerBatch, muteSource, unmuteSource, isSourceMuted, resetToDefaults],
  );
}

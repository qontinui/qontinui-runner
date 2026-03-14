/**
 * useWidgetPreferences Hook
 *
 * Manages widget pin/hide preferences stored in localStorage.
 * Pinned widgets are always shown; hidden widgets are filtered out.
 */

import { useState, useCallback, useMemo } from "react";
import type { ActivityType } from "../../types/dashboard/activity-types";
import { instanceStorage } from "@/lib/instance-storage";

const STORAGE_KEY = "qontinui-widget-prefs";

/**
 * Widget preferences stored in localStorage.
 */
interface WidgetPrefs {
  pinned: ActivityType[];
  hidden: ActivityType[];
}

/**
 * Result returned by useWidgetPreferences hook.
 */
export interface UseWidgetPreferencesResult {
  /** List of pinned widget types */
  pinned: ActivityType[];
  /** List of hidden widget types */
  hidden: ActivityType[];
  /** Pin a widget (always show it) */
  pinWidget: (type: ActivityType) => void;
  /** Unpin a widget */
  unpinWidget: (type: ActivityType) => void;
  /** Hide a widget (remove from display) */
  hideWidget: (type: ActivityType) => void;
  /** Unhide a widget */
  unhideWidget: (type: ActivityType) => void;
  /** Toggle pin state for a widget */
  togglePin: (type: ActivityType) => void;
  /** Check if a widget is pinned */
  isPinned: (type: ActivityType) => boolean;
  /** Check if a widget is hidden */
  isHidden: (type: ActivityType) => boolean;
  /** Apply preferences to a list of detected widgets */
  applyPreferences: (detected: ActivityType[]) => ActivityType[];
}

/**
 * Read preferences from instanceStorage.
 */
function readPrefs(): WidgetPrefs {
  const parsed = instanceStorage.getJSON<{ pinned?: unknown; hidden?: unknown } | null>(
    STORAGE_KEY,
    null,
  );
  if (parsed) {
    return {
      pinned: Array.isArray(parsed.pinned) ? parsed.pinned : [],
      hidden: Array.isArray(parsed.hidden) ? parsed.hidden : [],
    };
  }
  return { pinned: [], hidden: [] };
}

/**
 * Write preferences to instanceStorage.
 */
function writePrefs(prefs: WidgetPrefs): void {
  instanceStorage.setJSON(STORAGE_KEY, prefs);
}

/**
 * Hook for managing widget pin/hide preferences.
 */
export function useWidgetPreferences(): UseWidgetPreferencesResult {
  const [prefs, setPrefs] = useState<WidgetPrefs>(readPrefs);

  const updatePrefs = useCallback((updater: (prev: WidgetPrefs) => WidgetPrefs) => {
    setPrefs((prev) => {
      const next = updater(prev);
      writePrefs(next);
      return next;
    });
  }, []);

  const pinWidget = useCallback(
    (type: ActivityType) => {
      updatePrefs((prev) => ({
        pinned: prev.pinned.includes(type) ? prev.pinned : [...prev.pinned, type],
        // Remove from hidden if pinning
        hidden: prev.hidden.filter((t) => t !== type),
      }));
    },
    [updatePrefs],
  );

  const unpinWidget = useCallback(
    (type: ActivityType) => {
      updatePrefs((prev) => ({
        ...prev,
        pinned: prev.pinned.filter((t) => t !== type),
      }));
    },
    [updatePrefs],
  );

  const hideWidget = useCallback(
    (type: ActivityType) => {
      updatePrefs((prev) => ({
        // Remove from pinned if hiding
        pinned: prev.pinned.filter((t) => t !== type),
        hidden: prev.hidden.includes(type) ? prev.hidden : [...prev.hidden, type],
      }));
    },
    [updatePrefs],
  );

  const unhideWidget = useCallback(
    (type: ActivityType) => {
      updatePrefs((prev) => ({
        ...prev,
        hidden: prev.hidden.filter((t) => t !== type),
      }));
    },
    [updatePrefs],
  );

  const togglePin = useCallback(
    (type: ActivityType) => {
      if (prefs.pinned.includes(type)) {
        unpinWidget(type);
      } else {
        pinWidget(type);
      }
    },
    [prefs.pinned, pinWidget, unpinWidget],
  );

  const isPinned = useCallback((type: ActivityType) => prefs.pinned.includes(type), [prefs.pinned]);

  const isHidden = useCallback((type: ActivityType) => prefs.hidden.includes(type), [prefs.hidden]);

  const applyPreferences = useCallback(
    (detected: ActivityType[]): ActivityType[] => {
      // Filter out hidden, ensure pinned are included
      const filtered = detected.filter((t) => !prefs.hidden.includes(t));
      // Add pinned widgets that aren't already in the list
      for (const pinned of prefs.pinned) {
        if (!filtered.includes(pinned) && !prefs.hidden.includes(pinned)) {
          filtered.push(pinned);
        }
      }
      return filtered;
    },
    [prefs.pinned, prefs.hidden],
  );

  return useMemo(
    () => ({
      pinned: prefs.pinned,
      hidden: prefs.hidden,
      pinWidget,
      unpinWidget,
      hideWidget,
      unhideWidget,
      togglePin,
      isPinned,
      isHidden,
      applyPreferences,
    }),
    [
      prefs.pinned,
      prefs.hidden,
      pinWidget,
      unpinWidget,
      hideWidget,
      unhideWidget,
      togglePin,
      isPinned,
      isHidden,
      applyPreferences,
    ],
  );
}

export default useWidgetPreferences;

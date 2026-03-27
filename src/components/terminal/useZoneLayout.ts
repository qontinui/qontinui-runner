import { useState, useCallback, useEffect } from "react";
import { instanceStorage } from "@/lib/instance-storage";

// ── Layout Definitions ─────────────────────────────────────────────────────

export interface ZoneDefinition {
  /** CSS grid-column value */
  col: string;
  /** CSS grid-row value */
  row: string;
}

export interface LayoutPreset {
  id: string;
  name: string;
  /** Number of columns in the CSS grid */
  columns: number;
  /** Number of rows in the CSS grid */
  rows: number;
  zones: ZoneDefinition[];
  /** Shortcut key (1-9) for quick switching */
  shortcutKey?: number;
}

export const LAYOUT_PRESETS: LayoutPreset[] = [
  {
    id: "single",
    name: "Single",
    columns: 1,
    rows: 1,
    zones: [{ col: "1", row: "1" }],
    shortcutKey: 1,
  },
  {
    id: "split",
    name: "Split",
    columns: 2,
    rows: 1,
    zones: [
      { col: "1", row: "1" },
      { col: "2", row: "1" },
    ],
    shortcutKey: 2,
  },
  {
    id: "triptych",
    name: "Triptych",
    columns: 3,
    rows: 1,
    zones: [
      { col: "1", row: "1" },
      { col: "2", row: "1" },
      { col: "3", row: "1" },
    ],
    shortcutKey: 3,
  },
  {
    id: "quad",
    name: "Quad",
    columns: 2,
    rows: 2,
    zones: [
      { col: "1", row: "1" },
      { col: "2", row: "1" },
      { col: "1", row: "2" },
      { col: "2", row: "2" },
    ],
    shortcutKey: 4,
  },
  {
    id: "1-plus-4",
    name: "Focus + 4",
    columns: 4,
    rows: 4,
    zones: [
      { col: "1 / 3", row: "1 / 5" }, // large left (2×4)
      { col: "3 / 5", row: "1 / 3" }, // top-right (2×2)
      { col: "3 / 5", row: "3 / 5" }, // bottom-right (2×2)
    ],
    shortcutKey: 5,
  },
  {
    id: "six-pack",
    name: "Six Pack",
    columns: 3,
    rows: 2,
    zones: [
      { col: "1", row: "1" },
      { col: "2", row: "1" },
      { col: "3", row: "1" },
      { col: "1", row: "2" },
      { col: "2", row: "2" },
      { col: "3", row: "2" },
    ],
    shortcutKey: 6,
  },
  {
    id: "command-center",
    name: "Command Center",
    columns: 4,
    rows: 4,
    zones: [
      { col: "1 / 3", row: "1 / 5" }, // large left (2×4)
      { col: "3", row: "1" },
      { col: "4", row: "1" },
      { col: "3", row: "2" },
      { col: "4", row: "2" },
      { col: "3", row: "3" },
      { col: "4", row: "3" },
      { col: "3", row: "4" },
      { col: "4", row: "4" },
    ],
    shortcutKey: 7,
  },
  {
    id: "full-grid",
    name: "Full Grid",
    columns: 3,
    rows: 3,
    zones: [
      { col: "1", row: "1" },
      { col: "2", row: "1" },
      { col: "3", row: "1" },
      { col: "1", row: "2" },
      { col: "2", row: "2" },
      { col: "3", row: "2" },
      { col: "1", row: "3" },
      { col: "2", row: "3" },
      { col: "3", row: "3" },
    ],
    shortcutKey: 8,
  },
];

// ── Zone Assignment ────────────────────────────────────────────────────────

/** Maps zone index → tab ID */
export type ZoneAssignments = Record<number, string>;

// ── Session State (for status borders) ─────────────────────────────────────

export type SessionState = "idle" | "working" | "needs-input" | "completed" | "error";

const STORAGE_KEY = "qontinui-zone-layout";

interface PersistedState {
  layoutId: string;
  assignments: ZoneAssignments;
  focusedZone: number;
}

function loadPersistedState(): PersistedState | null {
  return instanceStorage.getJSON<PersistedState | null>(STORAGE_KEY, null);
}

function persistState(state: PersistedState) {
  try {
    instanceStorage.setJSON(STORAGE_KEY, state);
  } catch {
    // ignore storage errors
  }
}

// ── Hook ───────────────────────────────────────────────────────────────────

export function useZoneLayout(tabIds: string[]) {
  const [persistedState] = useState(() => loadPersistedState());

  const [layoutId, setLayoutIdState] = useState<string>(persistedState?.layoutId ?? "single");
  const [assignments, setAssignments] = useState<ZoneAssignments>(
    persistedState?.assignments ?? {},
  );
  const [focusedZone, setFocusedZone] = useState<number>(persistedState?.focusedZone ?? 0);
  /** Zone index that is temporarily maximized (null = normal grid) */
  const [maximizedZone, setMaximizedZone] = useState<number | null>(null);

  const layout = LAYOUT_PRESETS.find((l) => l.id === layoutId) ?? LAYOUT_PRESETS[0];

  // Persist on changes
  useEffect(() => {
    persistState({ layoutId, assignments, focusedZone });
  }, [layoutId, assignments, focusedZone]);

  // Auto-assign tabs to empty zones when tabs change
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- sync assignments when tab list changes
    setAssignments((prev) => {
      // Check if any removals or additions are needed before cloning
      const hasDeadAssignments = Object.values(prev).some((tabId) => !tabIds.includes(tabId));
      const assignedTabIds = new Set(Object.values(prev));
      const hasUnassigned = tabIds.some((id) => !assignedTabIds.has(id));

      if (!hasDeadAssignments && !hasUnassigned) return prev;

      const next = { ...prev };

      // Remove assignments for tabs that no longer exist
      if (hasDeadAssignments) {
        for (const [zoneIdx, tabId] of Object.entries(next)) {
          if (!tabIds.includes(tabId)) {
            delete next[Number(zoneIdx)];
          }
        }
      }

      // Find unassigned tabs and fill empty zone slots
      if (hasUnassigned) {
        const nowAssigned = new Set(Object.values(next));
        const unassigned = tabIds.filter((id) => !nowAssigned.has(id));
        const maxZones = layout.zones.length;
        for (let z = 0; z < maxZones && unassigned.length > 0; z++) {
          if (!(z in next) || !next[z]) {
            next[z] = unassigned.shift()!;
          }
        }
      }

      return next;
    });
  }, [tabIds, layout.zones.length]);

  const setLayoutId = useCallback(
    (id: string) => {
      const newLayout = LAYOUT_PRESETS.find((l) => l.id === id);
      if (!newLayout) return;

      setLayoutIdState(id);
      setMaximizedZone(null);

      // Reassign: keep existing assignments where zones exist, redistribute
      setAssignments((prev) => {
        const next: ZoneAssignments = {};
        const usedTabs = new Set<string>();

        // Keep assignments that fit in the new layout
        for (let z = 0; z < newLayout.zones.length; z++) {
          if (prev[z] && tabIds.includes(prev[z])) {
            next[z] = prev[z];
            usedTabs.add(prev[z]);
          }
        }

        // Fill remaining zones with unassigned tabs
        const unassigned = tabIds.filter((id) => !usedTabs.has(id));
        for (let z = 0; z < newLayout.zones.length && unassigned.length > 0; z++) {
          if (!(z in next)) {
            next[z] = unassigned.shift()!;
          }
        }

        return next;
      });

      // Clamp focused zone
      setFocusedZone((prev) => (prev >= newLayout.zones.length ? 0 : prev));
    },
    [tabIds],
  );

  const assignTabToZone = useCallback((zoneIndex: number, tabId: string) => {
    setAssignments((prev) => {
      const next = { ...prev };
      // If this tab is already in another zone, swap
      for (const [idx, id] of Object.entries(next)) {
        if (id === tabId && Number(idx) !== zoneIndex) {
          // Swap: put the displaced tab into the old slot
          const displaced = next[zoneIndex];
          if (displaced) {
            next[Number(idx)] = displaced;
          } else {
            delete next[Number(idx)];
          }
          break;
        }
      }
      next[zoneIndex] = tabId;
      return next;
    });
  }, []);

  const focusedTabId = assignments[focusedZone] ?? null;

  const focusNextZone = useCallback(() => {
    setFocusedZone((prev) => {
      const maxZones = Math.min(layout.zones.length, tabIds.length);
      if (maxZones <= 1) return prev;
      return (prev + 1) % maxZones;
    });
  }, [layout.zones.length, tabIds.length]);

  const focusPrevZone = useCallback(() => {
    setFocusedZone((prev) => {
      const maxZones = Math.min(layout.zones.length, tabIds.length);
      if (maxZones <= 1) return prev;
      return (prev - 1 + maxZones) % maxZones;
    });
  }, [layout.zones.length, tabIds.length]);

  const toggleMaximize = useCallback((zoneIndex?: number) => {
    setMaximizedZone((prev) => {
      const target = zoneIndex ?? 0;
      return prev === target ? null : target;
    });
  }, []);

  /** Get the list of tab IDs not assigned to any visible zone */
  const unassignedTabIds = tabIds.filter((id) => !Object.values(assignments).includes(id));

  /** Is this a multi-zone layout? */
  const isMultiZone = layout.zones.length > 1;

  /**
   * Focus the next zone whose session is in "needs-input" state.
   * Cycles starting from focusedZone + 1, wrapping around.
   * Returns true if a needs-input zone was found.
   */
  const focusNextNeedsInput = useCallback(
    (sessionStates: Record<string, SessionState>): boolean => {
      const maxZones = Math.min(layout.zones.length, tabIds.length);
      for (let i = 1; i <= maxZones; i++) {
        const candidate = (focusedZone + i) % maxZones;
        const tabId = assignments[candidate];
        if (tabId && sessionStates[tabId] === "needs-input") {
          setFocusedZone(candidate);
          // Also un-maximize so the user can see the zone
          setMaximizedZone(null);
          return true;
        }
      }
      // Fallback: try error states
      for (let i = 1; i <= maxZones; i++) {
        const candidate = (focusedZone + i) % maxZones;
        const tabId = assignments[candidate];
        if (tabId && sessionStates[tabId] === "error") {
          setFocusedZone(candidate);
          setMaximizedZone(null);
          return true;
        }
      }
      return false;
    },
    [layout.zones.length, tabIds.length, focusedZone, assignments],
  );

  return {
    layout,
    layoutId,
    setLayoutId,
    assignments,
    assignTabToZone,
    focusedZone,
    setFocusedZone,
    focusedTabId,
    focusNextZone,
    focusPrevZone,
    maximizedZone,
    setMaximizedZone,
    toggleMaximize,
    unassignedTabIds,
    isMultiZone,
    focusNextNeedsInput,
  };
}

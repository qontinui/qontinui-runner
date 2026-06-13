import { useState, useCallback, useEffect, useRef } from "react";
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

/** Largest preset by zone count — the auto-grow ceiling. `full-grid` (9). */
export const MAX_LAYOUT_ID = "full-grid";

/**
 * Pick the smallest layout preset whose zone count fits `totalTabs` live tabs,
 * capped at the largest preset (`full-grid`, 9 zones). Pure — exported so the
 * `+ new terminal` quick-launch path, the in-hook auto-grow effect, and the
 * unit test can all share ONE mapping (the prior copy lived inline in
 * `TerminalPage.tsx`).
 *
 * Mapping: 1→single, 2→split, 3-4→quad, 5-6→six-pack, ≥7→full-grid. Skips the
 * asymmetric presets (triptych / 1-plus-4 / command-center) deliberately — the
 * grow path wants the smallest *uniform* grid that fits, matching the legacy
 * inline behavior this extracted.
 */
export function pickLayout(totalTabs: number): string {
  if (totalTabs >= 7) return "full-grid";
  if (totalTabs >= 5) return "six-pack";
  if (totalTabs >= 3) return "quad";
  if (totalTabs >= 2) return "split";
  return "single";
}

/**
 * Decide whether the layout must GROW to fit `tabCount` live tabs, returning the
 * target layout id or `null` when no change is needed. Pure reducer behind the
 * in-hook auto-grow effect.
 *
 * Rules (all enforced here so the effect stays a thin wrapper, and the test can
 * assert them without React):
 *   - **grow only, never shrink**: only returns a target whose zone count is
 *     STRICTLY GREATER than the current layout's; fewer tabs → null.
 *   - **capacity-driven**: only grows when `tabCount` exceeds the current
 *     layout's zone capacity.
 *   - **capped at `full-grid`** (via `pickLayout`); at 9+ tabs the target is
 *     already `full-grid`, so once there it returns null (no thrash).
 *
 * There is deliberately NO operator-pinned escape hatch: every live session
 * must render in a zone. A pin latch used to suppress growth here, which let
 * gate-continuation sessions dock as invisible zoneless tabs behind a small
 * layout — operators ran blind to mid-implementation work for an hour.
 */
export function computeAutoGrowLayoutId(
  currentLayoutId: string,
  tabCount: number,
): string | null {
  const current = LAYOUT_PRESETS.find((l) => l.id === currentLayoutId) ?? LAYOUT_PRESETS[0];
  // Only act when live tabs overflow the current capacity.
  if (tabCount <= current.zones.length) return null;
  const targetId = pickLayout(tabCount);
  const target = LAYOUT_PRESETS.find((l) => l.id === targetId);
  if (!target) return null;
  // Grow only — never pick a target with fewer/equal zones than current.
  if (target.zones.length <= current.zones.length) return null;
  return targetId;
}

/**
 * Decide whether the layout must GROW so that `zoneIndex` is renderable,
 * returning the target layout id or `null` when the zone already fits (or no
 * larger preset exists). Pure sibling of [`computeAutoGrowLayoutId`], keyed by
 * a zone INDEX instead of the tab count (item 10 of the boot-restore
 * remediation): when restore excludes a record, the surviving records' zone
 * indices have GAPS (e.g. rows at zones 0 and 3 with 2 tabs), so the
 * count-based grow can pick a layout too small for the highest recorded zone
 * and `applyLayout` would silently compact it — breaking the operator's
 * spatial memory. Same grow-only / uniform-preset / `full-grid`-cap rules as
 * the count-based path; a zone index beyond the 9-zone ceiling stays
 * un-growable (the caller's compaction is then the honest fallback).
 */
export function computeZoneCapacityGrowth(
  currentLayoutId: string,
  zoneIndex: number,
): string | null {
  const current = LAYOUT_PRESETS.find((l) => l.id === currentLayoutId) ?? LAYOUT_PRESETS[0];
  if (zoneIndex < current.zones.length) return null;
  const targetId = pickLayout(zoneIndex + 1);
  const target = LAYOUT_PRESETS.find((l) => l.id === targetId);
  if (!target) return null;
  // Grow only — and only when the target actually renders the zone.
  if (target.zones.length <= current.zones.length || zoneIndex >= target.zones.length) {
    return null;
  }
  return targetId;
}

// ── Zone Assignment ────────────────────────────────────────────────────────

/** Maps zone index → tab ID */
export type ZoneAssignments = Record<number, string>;

// ── Session State (for status borders) ─────────────────────────────────────

export type SessionState = "idle" | "working" | "needs-input" | "completed" | "error";

const BASE_STORAGE_KEY = "qontinui-zone-layout";

interface PersistedState {
  layoutId: string;
  assignments: ZoneAssignments;
  focusedZone: number;
  // NOTE: older builds persisted a `pinned` latch here that suppressed
  // auto-grow. The concept is removed (it hid live sessions as zoneless
  // tabs); a stale `pinned` key in storage is simply ignored on load.
}

// Phase 1 (pop-out windows): pop-out windows share the main window's
// localStorage (same WebView2 profile per process), so each window's zone
// layout MUST be namespaced by its `windowLabel` or they clobber each other.
// `"main"` keeps the legacy key unchanged (existing layouts preserved).
function storageKey(pageId: string, windowLabel: string = "main"): string {
  const base = pageId === "default" ? BASE_STORAGE_KEY : `page:${pageId}:${BASE_STORAGE_KEY}`;
  return windowLabel === "main" ? base : `${base}:window:${windowLabel}`;
}

function loadPersistedState(pageId: string, windowLabel?: string): PersistedState | null {
  return instanceStorage.getJSON<PersistedState | null>(storageKey(pageId, windowLabel), null);
}

function persistState(pageId: string, windowLabel: string | undefined, state: PersistedState) {
  try {
    instanceStorage.setJSON(storageKey(pageId, windowLabel), state);
  } catch {
    // ignore storage errors
  }
}

// ── Pure assignment reconciliation ─────────────────────────────────────────

/**
 * Pure reducer behind the "auto-assign tabs to empty zones" effect. Given the
 * previous `zoneIndex → tabId` map, the current live `tabIds`, the layout's
 * zone count, and the set of zones RESERVED by a session record (claimed via
 * `assignTabToZone` during restore), produce the next assignment map:
 *
 *   - drop assignments for tabs that no longer exist,
 *   - creation-order auto-fill unassigned tabs into empty zones,
 *   - but NEVER fill a reserved-yet-empty zone (so a plain shell created first
 *     can't steal the zone a Claude session record claimed — the creation-order
 *     bug this registry fix targets).
 *
 * Returns `prev` (same identity) when nothing changed. Exported so the
 * regression test can assert the binding without booting React.
 */
export function reconcileAssignments(
  prev: ZoneAssignments,
  tabIds: string[],
  maxZones: number,
  reservedZones: ReadonlySet<number> = new Set(),
): ZoneAssignments {
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

  // Find unassigned tabs and fill empty zone slots. Skip zones a session
  // record has RESERVED so a plain shell created first can never steal a
  // Claude session's zone.
  if (hasUnassigned) {
    const nowAssigned = new Set(Object.values(next));
    const unassigned = tabIds.filter((id) => !nowAssigned.has(id));
    for (let z = 0; z < maxZones && unassigned.length > 0; z++) {
      if (reservedZones.has(z) && !next[z]) continue;
      if (!(z in next) || !next[z]) {
        next[z] = unassigned.shift()!;
      }
    }
  }

  return next;
}

// ── Hook ───────────────────────────────────────────────────────────────────

export function useZoneLayout(
  tabIds: string[],
  pageId: string = "default",
  windowLabel: string = "main",
) {
  const [persistedState] = useState(() => loadPersistedState(pageId, windowLabel));

  const [layoutId, setLayoutIdState] = useState<string>(persistedState?.layoutId ?? "single");
  const [assignments, setAssignments] = useState<ZoneAssignments>(
    persistedState?.assignments ?? {},
  );
  const [focusedZone, setFocusedZone] = useState<number>(persistedState?.focusedZone ?? 0);
  /** Zone index that is temporarily maximized (null = normal grid) */
  const [maximizedZone, setMaximizedZone] = useState<number | null>(null);

  // Zones explicitly claimed by a durable session record during restore.
  // The creation-order auto-fill below MUST NOT steal these even while the
  // record's tab hasn't landed in `tabIds` yet — otherwise a plain shell
  // created first grabs zone 0/3 and the real Claude session spills to an
  // unassigned slot (the creation-order bug this registry fix targets).
  // `assignTabToZone` registers the zone here so the guard holds across the
  // async restore window; the reservation is cleared once the zone is filled.
  const reservedZonesRef = useRef<Set<number>>(new Set());

  const layout = LAYOUT_PRESETS.find((l) => l.id === layoutId) ?? LAYOUT_PRESETS[0];

  // Persist on changes.
  useEffect(() => {
    persistState(pageId, windowLabel, {
      layoutId,
      assignments,
      focusedZone,
    });
  }, [pageId, windowLabel, layoutId, assignments, focusedZone]);

  // Auto-assign tabs to empty zones when tabs change
  useEffect(() => {
    setAssignments((prev) =>
      reconcileAssignments(prev, tabIds, layout.zones.length, reservedZonesRef.current),
    );
  }, [tabIds, layout.zones.length]);

  // Shared layout-application logic: switch the preset and redistribute
  // assignments. Used by BOTH the operator-facing `setLayoutId` and the
  // programmatic auto-grow effect.
  const applyLayout = useCallback(
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

  // ── Phase 1: auto-grow layout to fit live tabs ───────────────────────────
  // When the live tab count overflows the current layout's zone capacity, grow
  // the layout to the smallest preset that fits (capped at `full-grid`/9). Lives
  // HERE (in the hook, keyed on the live `tabIds.length` the hook already owns)
  // so it fires for EVERY ingest path — first mount/reconnect, `terminal-created`
  // events, gate-continuation ingest, and profile restore — not just the
  // `+ new terminal` button that grew the layout before.
  //
  // No render loop: `computeAutoGrowLayoutId` is GROW-ONLY and capacity-gated.
  // After it grows, `layout.zones.length >= tabIds.length`, so the next run of
  // this effect (re-keyed by the new `layoutId`) computes `null` and is inert.
  useEffect(() => {
    const target = computeAutoGrowLayoutId(layoutId, tabIds.length);
    if (target) {
      applyLayout(target);
    }
  }, [layoutId, tabIds.length, applyLayout]);

  /**
   * Operator-facing layout switch (picker UI / keyboard shortcut / `/layout`
   * command / profile + workspace load). The choice is a STARTING POINT, not a
   * latch: if live sessions later overflow the chosen layout's capacity,
   * auto-grow still expands it — every live session must render in a zone.
   */
  const setLayoutId = useCallback(
    (id: string) => {
      if (!LAYOUT_PRESETS.some((l) => l.id === id)) return;
      applyLayout(id);
    },
    [applyLayout],
  );

  const assignTabToZone = useCallback(
    (zoneIndex: number, tabId: string) => {
      // A recorded zone beyond the current layout's capacity must GROW the
      // layout so the binding survives (item 10 — restore with gaps: excluded
      // records leave the surviving zone indices sparse, so the count-based
      // auto-grow alone can pick a layout too small for the highest recorded
      // zone and the assignment would compact away). Beyond the full-grid
      // ceiling there is nothing to grow into — the reconcile/compaction
      // fallback then applies.
      const growTarget = computeZoneCapacityGrowth(layoutId, zoneIndex);
      if (growTarget) applyLayout(growTarget);
      // Reserve the zone so the creation-order auto-fill can't steal it while the
      // tab is mid-creation (restore path). Cleared in the setter below once the
      // zone holds its intended tab.
      if (zoneIndex >= 0) reservedZonesRef.current.add(zoneIndex);
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
        // The zone now holds its intended tab — drop the reservation so a later
        // genuine vacancy can be auto-filled normally.
        reservedZonesRef.current.delete(zoneIndex);
        return next;
      });
    },
    [layoutId, applyLayout],
  );

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

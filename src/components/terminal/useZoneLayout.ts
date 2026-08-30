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

// ── Flow-grid (past-9 scrolling synthesis) ─────────────────────────────────
//
// Phase 1 of the terminal flow-grid plan: the fixed `LAYOUT_PRESETS` cap out at
// `full-grid` (9 zones). Past 9 live tabs we STOP using presets and synthesize a
// scrolling flow-grid instead — a fixed 3-column grid whose rows grow downward
// so EVERY session gets a real, visible zone (no invisible offscreen tabs). The
// synthesized preset carries the reserved id {@link FLOW_GRID_ID}; it is never a
// member of `LAYOUT_PRESETS` (it's produced on demand by {@link resolveLayout}),
// so every layout-id lookup/validation site must special-case this id or route
// through `resolveLayout`, or the grow into flow mode silently no-ops.

/** Reserved id for the synthesized past-9 scrolling flow-grid (NOT in `LAYOUT_PRESETS`). */
export const FLOW_GRID_ID = "flow-grid";

/** Fixed column count for the flow-grid. Deferred: viewport-derived columns. */
export const FLOW_COLS = 3;

/**
 * Minimum flow-grid tile height in px (≈24 PTY rows at the default font). Below
 * this a terminal is decorative, so rows keep at least this height and the page
 * scrolls rather than shrinking tiles into uselessness.
 */
export const MIN_TILE_HEIGHT_PX = 300;

/**
 * Synthesize the scrolling flow-grid preset for `tabCount` live tabs: a fixed
 * {@link FLOW_COLS}-column grid with `ceil(tabCount / FLOW_COLS)` rows, one zone
 * per tab laid out left-to-right, top-to-bottom. Pure — the zone `col`/`row`
 * strings match the shape of the static `LAYOUT_PRESETS` zones (single-track
 * `"N"` values), so `ZoneGrid`/`ZoneCell` render them identically.
 *
 * Rows shrink naturally as tabs close: the id stays {@link FLOW_GRID_ID} and the
 * row count is derived from the CURRENT tab count, so this is NOT a grow-only
 * violation — it's the same layout id re-sized, not a shrink to a smaller preset.
 */
export function synthesizeFlowGrid(tabCount: number): LayoutPreset {
  const count = Math.max(tabCount, 1);
  const rows = Math.ceil(count / FLOW_COLS);
  const zones: ZoneDefinition[] = [];
  for (let i = 0; i < count; i++) {
    zones.push({
      col: String((i % FLOW_COLS) + 1),
      row: String(Math.floor(i / FLOW_COLS) + 1),
    });
  }
  return {
    id: FLOW_GRID_ID,
    name: "Flow Grid",
    columns: FLOW_COLS,
    rows,
    zones,
  };
}

/**
 * Resolve a layout id to a concrete preset: the static preset table UNION the
 * on-demand flow-grid synthesis. `FLOW_GRID_ID` synthesizes a preset sized to
 * `tabCount`; any other id is looked up in `LAYOUT_PRESETS`, falling back to the
 * first preset (`single`) for unknown ids. EVERY layout-id lookup that must
 * accept a persisted/auto-grown flow-grid id goes through here.
 */
export function resolveLayout(layoutId: string, tabCount: number): LayoutPreset {
  if (layoutId === FLOW_GRID_ID) return synthesizeFlowGrid(tabCount);
  return LAYOUT_PRESETS.find((l) => l.id === layoutId) ?? LAYOUT_PRESETS[0];
}

/**
 * Pick the smallest layout preset whose zone count fits `totalTabs` live tabs,
 * capped at the largest preset (`full-grid`, 9 zones). Pure — exported so the
 * `+ new terminal` quick-launch path, the in-hook auto-grow effect, and the
 * unit test can all share ONE mapping (the prior copy lived inline in
 * `TerminalPage.tsx`).
 *
 * Mapping: 1→single, 2→split, 3-4→quad, 5-6→six-pack, 7-9→full-grid,
 * ≥10→flow-grid (the synthesized past-9 scrolling grid — no fixed preset ceiling
 * anymore). Skips the asymmetric presets (triptych / 1-plus-4 / command-center)
 * deliberately — the grow path wants the smallest *uniform* grid that fits,
 * matching the legacy inline behavior this extracted.
 */
export function pickLayout(totalTabs: number): string {
  if (totalTabs >= 10) return FLOW_GRID_ID;
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
 *   - **grows INTO `flow-grid` at tab 10** (via `pickLayout`): the flow-grid
 *     synthesizes exactly `tabCount` zones, so it "always fits". Once IN
 *     flow-grid it returns null — the layout re-sizes itself to the live tab
 *     count by construction (zones == tabCount), so there is nothing to grow to
 *     and no thrash.
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
  // Already in flow-grid: `synthesizeFlowGrid(tabCount)` gives one zone per tab,
  // so the layout is always full-capacity — no grow target, no render loop.
  if (currentLayoutId === FLOW_GRID_ID) return null;
  const current = resolveLayout(currentLayoutId, tabCount);
  // Only act when live tabs overflow the current capacity.
  if (tabCount <= current.zones.length) return null;
  const targetId = pickLayout(tabCount);
  const target = resolveLayout(targetId, tabCount);
  // Grow only — never pick a target with fewer/equal zones than current.
  if (target.zones.length <= current.zones.length) return null;
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

/**
 * Public form of the per-(page, window) zone-layout storage key. Exported so the
 * pop-out-page action can COPY a page's layout from the main window's key to the
 * new pop-out window's key, preserving the grid preset + zone assignments when a
 * whole page is detached. Keep in lockstep with {@link storageKey}.
 */
export function zoneLayoutStorageKey(pageId: string, windowLabel: string = "main"): string {
  return storageKey(pageId, windowLabel);
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

/**
 * Pure reducer behind `applyLayout` (layout switch / auto-grow). Boot-restore
 * remediation item 10: a restored session's RECORDED zone must survive layout
 * application — compaction is allowed ONLY for assignments whose zone index
 * exceeds the new layout's zone count.
 *
 * The previous inline version kept an assignment only when
 * `tabIds.includes(prev[z])` — but `applyLayout` closes over `tabIds`, and
 * during the async restore drain that closure can be STALE (the tab exists
 * but isn't in the captured list yet). The assignment was silently dropped
 * and the creation-order auto-fill later re-placed the tab into the first
 * empty zone — the "record z3 rendered z2" drift. Dead-tab cleanup is NOT
 * this reducer's job: `reconcileAssignments` runs right after every layout
 * change with fresh `tabIds` and owns dropping dead assignments.
 *
 * Rules:
 *   - in-range assignments (`zone < zoneCount`) are preserved EXACTLY;
 *   - out-of-range assignments are compacted into the lowest empty zones
 *     (they had recorded zones, so they outrank plain unassigned tabs);
 *   - remaining empty zones are filled with unassigned tabs in creation
 *     order (same behavior as before).
 *
 * Exported so the restore regression test can assert the binding without
 * booting React (`useZoneLayout.restore.test.ts`).
 */
export function applyLayoutAssignments(
  prev: ZoneAssignments,
  tabIds: string[],
  zoneCount: number,
): ZoneAssignments {
  const next: ZoneAssignments = {};
  const overflow: string[] = [];
  const usedTabs = new Set<string>();

  for (const [z, tabId] of Object.entries(prev)) {
    const zone = Number(z);
    if (zone < zoneCount) {
      next[zone] = tabId;
      usedTabs.add(tabId);
    } else {
      overflow.push(tabId);
    }
  }

  // Compact ONLY the out-of-range tabs into the lowest empty zones.
  for (let z = 0; z < zoneCount && overflow.length > 0; z++) {
    if (!(z in next)) {
      const tabId = overflow.shift()!;
      next[z] = tabId;
      usedTabs.add(tabId);
    }
  }

  // Fill remaining zones with unassigned tabs (creation order).
  const unassigned = tabIds.filter((id) => !usedTabs.has(id));
  for (let z = 0; z < zoneCount && unassigned.length > 0; z++) {
    if (!(z in next)) {
      next[z] = unassigned.shift()!;
    }
  }

  return next;
}

// ── Hook ───────────────────────────────────────────────────────────────────

export function useZoneLayout(
  tabIds: string[],
  pageId: string = "default",
  windowLabel: string = "main",
  /**
   * IDs of tabs whose process is still alive. Used to split the exposed
   * unassigned set into hidden-but-LIVE (`unassignedTabIds`) vs exited
   * tombstones (`exitedUnassignedTabIds`) so dead sessions don't inflate the
   * UnzonedChip / control-panel "N more" honesty count. Defaults to "all tabs
   * live" when omitted, preserving prior behavior for any caller that doesn't
   * thread liveness. Assignment/reconcile logic keeps using the full `tabIds`
   * — a dead-but-assigned tab still renders a tombstone card in its zone.
   */
  liveTabIds?: ReadonlySet<string>,
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

  // Resolve through `resolveLayout` so a persisted/auto-grown `flow-grid` id
  // materializes a synthesized preset sized to the CURRENT live tab count
  // (zones == tabIds.length in flow mode), instead of silently falling back to
  // `single` and re-hiding every session past the 9th.
  const layout = resolveLayout(layoutId, tabIds.length);

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
      // `resolveLayout` accepts the synthetic `flow-grid` id (auto-grow calls
      // `applyLayout` directly, so a plain `LAYOUT_PRESETS.find` returning
      // undefined would EAT the grow into flow mode). Unknown ids resolve to
      // `single` rather than no-op — `setLayoutId` already gates bad input.
      const newLayout = resolveLayout(id, tabIds.length);

      setLayoutIdState(id);
      setMaximizedZone(null);

      // Reassign: preserve every in-range assignment exactly; compact only
      // out-of-range ones; fill remaining zones with unassigned tabs. See
      // `applyLayoutAssignments` (item 10 — recorded zones must survive
      // layout application; dead-tab cleanup belongs to
      // `reconcileAssignments`, which runs with fresh tabIds).
      setAssignments((prev) => applyLayoutAssignments(prev, tabIds, newLayout.zones.length));

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
      // Accept the synthetic flow-grid id alongside the static presets — a
      // persisted/auto-grown `flow-grid` must be re-applyable, else restoring
      // into flow mode silently drops back to a preset and re-hides sessions.
      if (id !== FLOW_GRID_ID && !LAYOUT_PRESETS.some((l) => l.id === id)) return;
      applyLayout(id);
    },
    [applyLayout],
  );

  const assignTabToZone = useCallback((zoneIndex: number, tabId: string) => {
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
  }, []);

  const focusedTabId = assignments[focusedZone] ?? null;

  /**
   * The number of zones focus can actually cycle through.
   *
   * It is `min(zones, tabs)`, NOT the zone count: a four-zone grid holding one
   * session has nowhere to move focus to. That distinction is the reason these
   * two report — `/focus next` cannot compute the cap for itself without
   * copying this expression, and a copied cap is exactly the kind of drift
   * this pipeline keeps rediscovering. One place owns it, and it says whether
   * focus moved.
   */
  const focusableZoneCount = Math.min(layout.zones.length, tabIds.length);

  const focusNextZone = useCallback((): { changed: boolean } => {
    if (focusableZoneCount <= 1) return { changed: false };
    setFocusedZone((prev) => (prev + 1) % focusableZoneCount);
    return { changed: true };
  }, [focusableZoneCount]);

  const focusPrevZone = useCallback((): { changed: boolean } => {
    if (focusableZoneCount <= 1) return { changed: false };
    setFocusedZone((prev) => (prev - 1 + focusableZoneCount) % focusableZoneCount);
    return { changed: true };
  }, [focusableZoneCount]);

  const toggleMaximize = useCallback((zoneIndex?: number) => {
    setMaximizedZone((prev) => {
      const target = zoneIndex ?? 0;
      return prev === target ? null : target;
    });
  }, []);

  /**
   * Unassigned tab IDs, split by liveness. `unassignedTabIds` is the
   * hidden-but-LIVE set that the UnzonedChip / control-panel "N more" count is
   * contractually about — exited tombstones are excluded so a dead session
   * never inflates it. `exitedUnassignedTabIds` surfaces those tombstones
   * separately (e.g. "N exited") and drives the dismiss affordance. When no
   * liveness set is threaded, every tab is treated as live (prior behavior).
   */
  const assignedTabIdSet = new Set(Object.values(assignments));
  const isLive = (id: string) => (liveTabIds ? liveTabIds.has(id) : true);
  const unassignedTabIds = tabIds.filter((id) => !assignedTabIdSet.has(id) && isLive(id));
  const exitedUnassignedTabIds = tabIds.filter((id) => !assignedTabIdSet.has(id) && !isLive(id));

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
    exitedUnassignedTabIds,
    isMultiZone,
    focusNextNeedsInput,
  };
}

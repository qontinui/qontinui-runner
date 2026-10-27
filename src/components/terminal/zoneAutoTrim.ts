/**
 * Auto-trim: a page whose grid the RUNNER chose shrinks back when its tabs
 * close, the mirror of auto-grow (`computeAutoGrowLayoutId`).
 *
 * Grow-only protected two things, and both still hold:
 *   1. every live session renders in a zone — the trim target is
 *      `pickLayout(tabCount)`, which always has at least one zone per tab;
 *   2. the operator's own choice of layout — only a layout whose origin is
 *      `auto` (grown, quick-launched, compacted or trimmed by the runner) is
 *      ever trimmed. One the operator picked (`operator`), and one persisted
 *      before the origin existed (absent), is left alone.
 *
 * The decision is pure and lives here; `useZoneLayout` keeps only the effect
 * that feeds it (a tab-count decrease latches a trim as owed, and the owed
 * trim applies once no {@link autoTrimHold} is active).
 *
 * Zone labels, notes, pins and output filters are keyed by zone index, so a
 * runner-originated switch that moves tiles carries them by TAB
 * ({@link migrateZoneMetadataByTab}): old zone → tab → new zone.
 */

import { instanceStorage } from "@/lib/instance-storage";
import type { TerminalWorkMode } from "./terminalWorkMode";
import { pickLayout, resolveLayout, type ZoneAssignments } from "./useZoneLayout";

/**
 * Who chose the page's current layout. `auto`: the runner (auto-grow, a quick
 * or AI launch, a compaction, the trim). `operator`: the operator (picker,
 * chords, `/layout`, profile or workspace load, a preset pick out of tmux).
 * Persisted on the page's zone-layout state; ABSENT means the page predates
 * the field, and reads as `operator` to the trim.
 */
export type LayoutOrigin = "auto" | "operator";

/** Read a persisted origin: a known value, else absent (never guessed). */
export function parseLayoutOrigin(raw: unknown): LayoutOrigin | undefined {
  return raw === "auto" || raw === "operator" ? raw : undefined;
}

/**
 * The layout to trim to after a tab-count decrease, or `null` to leave the
 * layout alone. Returns `pickLayout(tabCount)` — what auto-grow would have
 * grown to for this many tabs, so a tab spawned right after a trim needs no
 * re-flow — only when ALL of these hold:
 *   - the runner chose the current layout (`origin === "auto"`; absent reads
 *     as `operator`);
 *   - `tabCount >= 1`: a page this window owns no tabs of (popped out to its
 *     own window) is "not here", not "empty";
 *   - not tmux work mode, whose layout is pinned to `single`;
 *   - the target has STRICTLY fewer zones than `currentZoneCount`, the zone
 *     count the page renders now (for flow-grid, its `flowGridSlotCount`
 *     size, not the tab count);
 *   - the target is a different layout. Flow-grid at 10+ tabs stays
 *     flow-grid: it already re-sizes to its highest occupied tile, and
 *     re-packing its survivors into the holes is not a trim.
 */
export function computeAutoTrimLayoutId(
  currentLayoutId: string,
  origin: LayoutOrigin | undefined,
  tabCount: number,
  currentZoneCount: number,
  workMode: TerminalWorkMode,
): string | null {
  if ((origin ?? "operator") !== "auto") return null;
  if (tabCount < 1) return null;
  if (workMode === "tmux") return null;
  const targetId = pickLayout(tabCount);
  if (targetId === currentLayoutId) return null;
  if (resolveLayout(targetId, tabCount).zones.length >= currentZoneCount) return null;
  return targetId;
}

/** Why an owed trim is waiting, in the order they are checked. */
export type AutoTrimHold =
  /** The page's restore has not settled (`useTerminalManager().initialized`). */
  | "restoring"
  /** A session record has claimed a zone its tab has not filled yet. */
  | "zone-reserved"
  /** A "keep AI sessions" compaction is about to re-deal every zone. */
  | "compaction-pending"
  /** Re-applying a layout un-maximizes; the operator is looking at one zone. */
  | "maximized"
  /** tmux work mode: the layout is pinned to `single`, the grid is a snapshot. */
  | "tmux";

export interface AutoTrimHoldInputs {
  initialized: boolean;
  reservedZoneCount: number;
  compactionPending: boolean;
  maximizedZone: number | null;
  workMode: TerminalWorkMode;
}

/**
 * The hold keeping an owed trim from applying, or `null` when it may apply.
 * A trim during the restore drain would push a recorded zone past the new
 * zone count and compact it into a hole (the "record z3 rendered z2" drift);
 * the other holds are a re-deal about to happen anyway, a maximized view that
 * a layout switch would clear, and tmux mode.
 */
export function autoTrimHold(inputs: AutoTrimHoldInputs): AutoTrimHold | null {
  if (!inputs.initialized) return "restoring";
  if (inputs.reservedZoneCount > 0) return "zone-reserved";
  if (inputs.compactionPending) return "compaction-pending";
  if (inputs.maximizedZone !== null) return "maximized";
  if (inputs.workMode === "tmux") return "tmux";
  return null;
}

/** Per-runner-instance preference: shrink the grid when tabs close. */
export const AUTO_TRIM_STORAGE_KEY = "qontinui-zone-auto-trim";

/**
 * Is auto-trim on? Absent (or unreadable) means ON; only a stored `false`
 * turns it off. Read at the moment a trim is about to apply rather than held
 * in React state, so a flip needs no cross-page broadcast.
 */
export function readAutoTrimEnabled(): boolean {
  try {
    return instanceStorage.getJSON<unknown>(AUTO_TRIM_STORAGE_KEY, true) !== false;
  } catch {
    return true;
  }
}

export function writeAutoTrimEnabled(enabled: boolean): void {
  try {
    instanceStorage.setJSON(AUTO_TRIM_STORAGE_KEY, enabled);
  } catch {
    // ignore storage errors: the preference stays at its previous value
  }
}

/**
 * Carry zone-indexed metadata (labels, notes, pins, output filters) across an
 * assignment change by TAB: an entry on a zone whose tab now sits in another
 * zone moves there; an entry on a zone whose tab is gone (closed, or left
 * without a zone) is dropped, because it belonged to that session. An entry
 * on a zone that held no tab stays at its index, unless a moved entry lands
 * there — the session's own metadata wins.
 *
 * Returns `meta` itself when nothing moved, so a caller's state keeps its
 * identity.
 */
export function migrateZoneMetadataByTab<T>(
  meta: Readonly<Record<number, T>>,
  prev: ZoneAssignments,
  next: ZoneAssignments,
): Record<number, T> {
  const zoneOfTab = new Map<string, number>();
  for (const [zone, tabId] of Object.entries(next)) {
    if (tabId) zoneOfTab.set(tabId, Number(zone));
  }
  const moved: Record<number, T> = {};
  const unanchored: Array<[number, T]> = [];
  let changed = false;
  for (const [z, value] of Object.entries(meta) as Array<[string, T]>) {
    const zone = Number(z);
    const tabId = prev[zone];
    if (!tabId) {
      unanchored.push([zone, value]);
      continue;
    }
    const target = zoneOfTab.get(tabId);
    if (target === undefined) {
      changed = true;
      continue;
    }
    if (target !== zone) changed = true;
    moved[target] = value;
  }
  for (const [zone, value] of unanchored) {
    if (zone in moved) {
      changed = true;
      continue;
    }
    moved[zone] = value;
  }
  return changed ? moved : (meta as Record<number, T>);
}

/** {@link migrateZoneMetadataByTab} for a set of zone indices (pins). */
export function migrateZoneSetByTab(
  zones: ReadonlySet<number>,
  prev: ZoneAssignments,
  next: ZoneAssignments,
): Set<number> {
  const asRecord: Record<number, true> = {};
  for (const z of zones) asRecord[z] = true;
  const migrated = migrateZoneMetadataByTab(asRecord, prev, next);
  return new Set(Object.keys(migrated).map(Number));
}

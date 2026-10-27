/**
 * The zone-layout preset table, the past-9 flow-grid synthesis and the grow
 * ladder (`pickLayout`). A leaf module: pure data and functions with no
 * imports, so the hook (`useZoneLayout.ts`), auto-trim (`zoneAutoTrim.ts`) and
 * work mode (`terminalWorkMode.ts`) can all depend on it without a cycle.
 */

/** Maps zone index → tab ID */
export type ZoneAssignments = Record<number, string>;

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
 * The caller passes {@link flowGridSlotCount}, not the bare tab count, so the
 * grid always reaches the highest assigned zone. Rows shrink as the highest
 * tiles close: the id stays {@link FLOW_GRID_ID}, so this is NOT a grow-only
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
 * How many zones the flow-grid must synthesize: one per live tab, and never
 * fewer than it takes to reach the highest zone a live tab is assigned to.
 *
 * Sizing by tab count alone made any assignment at or past `tabIds.length`
 * point at a zone that was never rendered — and `classifyTabs` still counts
 * such a tab as assigned, so it got neither a zone nor the hidden mount: alive,
 * holding its PTY, and drawn nowhere. Closing any tile but the last one did
 * exactly that to the last tile. A restore that skips a record (so the tabs
 * that did come back hold zones past the new count) did it too. Covering the
 * highest assigned zone makes that state unrepresentable.
 *
 * A closed tile therefore leaves an empty tile, exactly as a close leaves an
 * empty zone in the preset layouts, and the next new tab fills the lowest one
 * (`reconcileAssignments`). A close alone re-packs nothing, and the grid
 * still shrinks whenever the highest tiles close. Survivors move only on a
 * layout switch: the operator's, or a runner-originated one (auto-trim
 * leaving flow-grid for `full-grid` at 9 tabs or fewer, `zoneAutoTrim.ts`;
 * a compaction). Zone labels, notes and pins are keyed by zone index, so a
 * runner-originated switch carries them by TAB (`migrateZoneMetadataByTab`)
 * rather than letting a moved tile wear another tile's metadata; output
 * filters follow their tab on every assignment change (`ZoneGrid`).
 *
 * Dead assignments (a tab already gone from `tabIds`, not yet dropped by the
 * reconcile effect) are ignored, so a closing tab cannot hold the grid open.
 */
export function flowGridSlotCount(tabIds: readonly string[], assignments: ZoneAssignments): number {
  const live = new Set(tabIds);
  let highest = -1;
  for (const [zone, tabId] of Object.entries(assignments)) {
    if (tabId && live.has(tabId)) highest = Math.max(highest, Number(zone));
  }
  return Math.max(tabIds.length, highest + 1);
}

/**
 * Is `layoutId` one the page can lay out: a static preset or the synthesized
 * flow grid. Anything else would silently resolve to `single`.
 */
export function isKnownLayoutId(layoutId: string): boolean {
  return layoutId === FLOW_GRID_ID || LAYOUT_PRESETS.some((l) => l.id === layoutId);
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
 * growing past the largest fixed preset (`full-grid`, 9 zones) into the
 * synthesized `flow-grid`. Pure — exported so EVERY consumer shares ONE
 * mapping: the quick-launch decision (`computeQuickLaunchLayoutId` in
 * `useZoneLayout.ts`, called from `useZoneActions` for `+ new terminal` and
 * from `TerminalPage.tsx` for the tab-bar quick launch and AI launch), the
 * in-hook auto-grow effect (`computeAutoGrowLayoutId` in `useZoneLayout.ts`),
 * the in-hook auto-trim effect (`computeAutoTrimLayoutId` in `zoneAutoTrim.ts`: the trim target is
 * what auto-grow would have grown to, so a spawn right after a trim needs no
 * re-flow), the "keep AI sessions" compaction, tmux mode's snapshot-less
 * exit, the layout-mismatch suggestion chip (`suggestions/rules.ts`), and the
 * unit tests.
 * Inline copies once lived in `TerminalPage.tsx`, `useZoneActions.ts` and
 * `suggestions/rules.ts`; the latter two had drifted (no flow-grid rung).
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

import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { instanceStorage } from "@/lib/instance-storage";
import { pageWindowStorageKey } from "./pageWindowStorageKey";
import {
  gridZoneOfTab,
  mruOnEnter,
  parseGridSnapshot,
  parseMru,
  parseWorkMode,
  placeInGridSnapshot,
  pushMru,
  reconcileTmuxAssignments,
  rekeyGridSnapshot,
  rekeyMru,
  replaceTabInAssignments,
  replaceTabInGridSnapshot,
  replaceTabInMru,
  restoreGridSnapshot,
  sessionKeysFor,
  shownTabOnEnter,
  tabsBySessionKey,
  type GridSnapshot,
  type LayoutRequestSource,
  type SessionKeyOf,
  type TerminalWorkMode,
} from "./terminalWorkMode";
import {
  autoTrimHold,
  computeAutoTrimLayoutId,
  parseLayoutOrigin,
  readAutoTrimEnabled,
  INITIAL_LAYOUT_SWITCH,
  type LayoutOrigin,
  type LayoutSwitch,
} from "./zoneAutoTrim";

// The preset table, flow-grid synthesis and the grow ladder live in the leaf
// module `zoneLayoutPresets.ts` (so `zoneAutoTrim.ts` can use them without an
// import cycle); re-exported here for the many existing importers.
export {
  FLOW_COLS,
  FLOW_GRID_ID,
  LAYOUT_PRESETS,
  MAX_LAYOUT_ID,
  MIN_TILE_HEIGHT_PX,
  flowGridSlotCount,
  isKnownLayoutId,
  pickLayout,
  resolveLayout,
  synthesizeFlowGrid,
  type LayoutPreset,
  type ZoneAssignments,
  type ZoneDefinition,
} from "./zoneLayoutPresets";
import {
  FLOW_GRID_ID,
  LAYOUT_PRESETS,
  flowGridSlotCount,
  isKnownLayoutId,
  pickLayout,
  resolveLayout,
  type ZoneAssignments,
} from "./zoneLayoutPresets";

/**
 * Decide whether the layout must GROW to fit `tabCount` live tabs, returning the
 * target layout id or `null` when no change is needed. Pure reducer behind the
 * in-hook auto-grow effect.
 *
 * Rules (all enforced here so the effect stays a thin wrapper, and the test can
 * assert them without React):
 *   - **this reducer only grows**: it only returns a target whose zone count
 *     is STRICTLY GREATER than the current layout's; fewer tabs → null. The
 *     shrink is a separate decision with its own guards
 *     (`computeAutoTrimLayoutId`, `zoneAutoTrim.ts`): it trims only a layout
 *     the runner chose, only after a tab-count decrease, and only once no
 *     restore, compaction, maximize or tmux hold is active, so an operator's
 *     roomy layout is never shrunk under them.
 *   - **capacity-driven**: only grows when `tabCount` exceeds the current
 *     layout's zone capacity.
 *   - **grows INTO `flow-grid` at tab 10** (via `pickLayout`): the flow-grid
 *     synthesizes at least `tabCount` zones, so it "always fits". Once IN
 *     flow-grid it returns null — the layout re-sizes itself to the live tabs
 *     by construction (zones >= tabCount, see `flowGridSlotCount`), so there
 *     is nothing to grow to and no thrash.
 *
 * There is deliberately NO operator-pinned escape hatch IN GRID MODE: every
 * live session must render in a zone. A pin latch used to suppress growth
 * here, which let gate-continuation sessions dock as invisible zoneless tabs
 * behind a small layout — operators ran blind to mid-implementation work for
 * an hour. tmux mode is the explicit opposite (one session on screen, the rest
 * reached from the session manager), so it never grows: `null`.
 */
export function computeAutoGrowLayoutId(
  currentLayoutId: string,
  tabCount: number,
  workMode: TerminalWorkMode,
): string | null {
  if (workMode === "tmux") return null;
  // Already in flow-grid: it synthesizes at least one zone per tab, so the
  // layout is always full-capacity — no grow target, no render loop.
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

/**
 * The `+ new terminal` quick-launch layout decision (`useZoneActions`'
 * `createAndAssignTerminal`): given the layout BEFORE the spawn and the tab
 * count AFTER it, return the layout id to switch to, or `null` to leave the
 * layout alone.
 *
 * It switches only when the new tab has nowhere to go (`totalTabs` exceeds the
 * zone count, or every zone is already occupied), and the target is always the
 * canonical {@link pickLayout} — this used to be a fourth inline copy of the
 * ladder with no `>= 10 → flow-grid` rung, so from the 11th tab on it picked
 * `full-grid` while the layout was already `flow-grid`: the layout SHRANK
 * (11 zones → 9), `applyLayoutAssignments` compacted every tab at zone ≥ 9 into
 * whatever holes existed, and the auto-grow effect regrew the layout but not
 * the arrangement — one silent re-shuffle of the operator's grid per spawn.
 *
 * Grow-only: a target with fewer zones than the current layout is refused
 * (`null`), so a spawn never compacts assignments. Shrinking is auto-trim's
 * job alone (`zoneAutoTrim.ts`), which runs after a close, never on a spawn,
 * and carries zone metadata by tab when it moves a tile.
 */
export function computeQuickLaunchLayoutId(
  currentLayoutId: string,
  currentZoneCount: number,
  hasEmptyZone: boolean,
  totalTabs: number,
): string | null {
  const needsRoom = totalTabs > currentZoneCount || (!hasEmptyZone && totalTabs > 1);
  if (!needsRoom) return null;
  const target = pickLayout(totalTabs);
  if (target === currentLayoutId) return null;
  if (resolveLayout(target, totalTabs).zones.length < currentZoneCount) return null;
  return target;
}

// ── Zone Assignment ────────────────────────────────────────────────────────

/** Shared empty assignments, so a memo over "no snapshot" keeps its identity. */
const EMPTY_ASSIGNMENTS: ZoneAssignments = {};

// ── Session State (for status borders) ─────────────────────────────────────

export type SessionState = "idle" | "working" | "needs-input" | "completed" | "error";

const BASE_STORAGE_KEY = "qontinui-zone-layout";

interface PersistedState {
  layoutId: string;
  assignments: ZoneAssignments;
  focusedZone: number;
  /** Absent on pages saved before the mode existed — read as `grid`. */
  workMode?: TerminalWorkMode;
  /** tmux mode's most-recently-shown tab ids, newest first. */
  mru?: string[];
  /**
   * The durable session key (`claudeSessionId`) of each MRU entry that has one
   * — the MRU head is the shown tab — so the shown session and the MRU follow
   * their sessions into the new tabs a runner restart resumes them in.
   */
  mruSessionKeys?: Record<string, string>;
  /** The grid to restore when tmux mode exits (`null` in grid mode). */
  gridSnapshot?: GridSnapshot | null;
  /**
   * Who chose `layoutId` (`zoneAutoTrim.ts`): only an `auto` layout is
   * trimmed when tabs close. Absent on pages saved before the field existed,
   * which read as `operator`, so no page shrinks on upgrade.
   */
  layoutOrigin?: LayoutOrigin;
  // NOTE: older builds persisted a `pinned` latch here that suppressed
  // auto-grow. The concept is removed (it hid live sessions as zoneless
  // tabs); a stale `pinned` key in storage is simply ignored on load.
}

// Phase 1 (pop-out windows): pop-out windows share the main window's
// localStorage (same WebView2 profile per process), so each window's zone
// layout MUST be namespaced by its `windowLabel` or they clobber each other.
// `"main"` keeps the legacy key unchanged (existing layouts preserved).
function storageKey(pageId: string, windowLabel: string = "main"): string {
  return pageWindowStorageKey(BASE_STORAGE_KEY, pageId, windowLabel);
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

/**
 * The work-mode half of a loaded state. A malformed `workMode` falls back to
 * grid and is logged with the value, so a corrupted preference is visible
 * rather than silently becoming either mode.
 */
function loadWorkModeState(persisted: PersistedState | null): {
  workMode: TerminalWorkMode;
  mru: string[];
  mruSessionKeys: Record<string, string>;
  gridSnapshot: GridSnapshot | null;
} {
  const raw = persisted?.workMode as unknown;
  const { mode, malformed } = parseWorkMode(raw);
  if (malformed) {
    console.warn(
      `[useZoneLayout] persisted workMode ${JSON.stringify(raw)} is not a known mode; using "grid"`,
    );
  }
  const rawSnapshot = persisted?.gridSnapshot as unknown;
  const gridSnapshot = parseGridSnapshot(rawSnapshot, isKnownLayoutId);
  if (gridSnapshot === null && rawSnapshot !== undefined && rawSnapshot !== null) {
    // Unreadable is not "no snapshot" silently: exiting tmux will fall back to
    // `pickLayout(tabCount)`, and the log says why the grid was re-dealt.
    console.warn(
      `[useZoneLayout] persisted gridSnapshot ${JSON.stringify(rawSnapshot)} is malformed; ignoring it`,
    );
  }
  return {
    workMode: mode,
    mru: parseMru(persisted?.mru),
    mruSessionKeys: parseMruSessionKeys(persisted?.mruSessionKeys),
    gridSnapshot,
  };
}

/** Read persisted MRU session keys: a tab-id → string map, anything else is empty. */
function parseMruSessionKeys(raw: unknown): Record<string, string> {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return {};
  const out: Record<string, string> = {};
  for (const [tabId, sessionKey] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof sessionKey === "string") out[tabId] = sessionKey;
  }
  return out;
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

/** Dense zone assignments for `orderedTabIds`: the i-th tab owns zone i. */
export function compactAssignments(orderedTabIds: readonly string[]): ZoneAssignments {
  const next: ZoneAssignments = {};
  orderedTabIds.forEach((id, i) => {
    next[i] = id;
  });
  return next;
}

/**
 * How long a compaction request waits for its closes. `closeTerminal` drops
 * the tab from state synchronously, so this never fires today; it bounds a
 * future close that does not, which would otherwise re-lay-out the grid as a
 * surprise whenever that tab finally went.
 */
const COMPACTION_TIMEOUT_MS = 5_000;

// ── Hook ───────────────────────────────────────────────────────────────────

/**
 * Unassigned tab IDs, split by liveness. `unassignedTabIds` is the
 * hidden-but-LIVE set that the UnzonedChip / control-panel "N more" count is
 * contractually about — exited tombstones are excluded so a dead session
 * never inflates it. `exitedUnassignedTabIds` surfaces those tombstones
 * separately (e.g. "N exited") and drives the dismiss affordance. When no
 * liveness set is threaded, every tab is treated as live (prior behavior).
 *
 * The ONE derivation of this split: `useZoneLayout` exposes it, and every
 * surface (TerminalPage's UnzonedChip, ZoneControlPanel's Unassigned / Exited
 * sections) consumes the hook's value rather than re-deriving it. Order
 * follows `tabIds`.
 */
export function partitionUnassignedTabIds(
  tabIds: readonly string[],
  assignments: ZoneAssignments,
  liveTabIds?: ReadonlySet<string>,
): { unassignedTabIds: string[]; exitedUnassignedTabIds: string[] } {
  const assignedTabIdSet = new Set(Object.values(assignments));
  const isLive = (id: string) => (liveTabIds ? liveTabIds.has(id) : true);
  const unassignedTabIds: string[] = [];
  const exitedUnassignedTabIds: string[] = [];
  for (const id of tabIds) {
    if (assignedTabIdSet.has(id)) continue;
    if (isLive(id)) unassignedTabIds.push(id);
    else exitedUnassignedTabIds.push(id);
  }
  return { unassignedTabIds, exitedUnassignedTabIds };
}

/**
 * The zone walk every focus cycler shares: starting AFTER `focusedZone` and
 * wrapping (the last candidate is `focusedZone` itself), return the first zone
 * index in `[0, zoneCount)` holding an assigned tab that satisfies `matches`,
 * or `null` if none does.
 *
 * It walks every zone of the layout and skips unassigned slots. It must NOT be
 * clamped to `min(zoneCount, tabCount)`: `reconcileAssignments` does not keep
 * assignments dense — per-zone tab pickers, drag-and-drop, `/swap`, Ctrl-swap
 * and closing the tabs in low zones all leave tabs at high indices — so a
 * clamp strands exactly those tabs.
 */
export function findNextZone(
  zoneCount: number,
  focusedZone: number,
  assignments: ZoneAssignments,
  matches: (tabId: string) => boolean,
  direction: 1 | -1 = 1,
): number | null {
  if (zoneCount <= 0) return null;
  for (let i = 1; i <= zoneCount; i++) {
    const candidate = (((focusedZone + direction * i) % zoneCount) + zoneCount) % zoneCount;
    const tabId = assignments[candidate];
    if (tabId && matches(tabId)) return candidate;
  }
  return null;
}

/**
 * Resolve tab ids (e.g. `unassignedTabIds`) back to their tab objects, in id
 * order, dropping any id with no matching tab.
 */
export function tabsForIds<T extends { id: string }>(
  ids: readonly string[],
  tabs: readonly T[],
): T[] {
  const byId = new Map(tabs.map((t) => [t.id, t] as const));
  const out: T[] = [];
  for (const id of ids) {
    const tab = byId.get(id);
    if (tab) out.push(tab);
  }
  return out;
}

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
   * tmux mode reads it too: a new shown tab is picked from the live tabs
   * before any exited one.
   */
  liveTabIds?: ReadonlySet<string>,
  /**
   * Each tab's durable session key (its `claudeSessionId`), for tabs that have
   * one. tmux mode's grid snapshot records it per zone so a session keeps its
   * grid zone across a runner restart, which re-creates its tab under a new id.
   */
  sessionKeyByTab?: ReadonlyMap<string, string>,
  /**
   * Has this page's restore settled (`useTerminalManager().initialized`)?
   * Until it has, tab-count changes are the restore drain, not closes, so an
   * owed auto-trim waits. Defaults to settled for callers with no restore.
   */
  initialized: boolean = true,
) {
  const [persistedState] = useState(() => loadPersistedState(pageId, windowLabel));
  const [initialMode] = useState(() => loadWorkModeState(persistedState));

  /**
   * The page's work mode (see `terminalWorkMode.ts`). In tmux mode the layout
   * is pinned to `single` and zone 0 is the shown tab, so a page reloaded in
   * tmux comes back pinned even if an older layout id was stored with it.
   */
  const [workMode, setWorkMode] = useState<TerminalWorkMode>(initialMode.workMode);
  const [mru, setMru] = useState<string[]>(initialMode.mru);
  const [mruSessionKeys, setMruSessionKeys] = useState<Record<string, string>>(
    initialMode.mruSessionKeys,
  );
  /**
   * The session that was on screen when a tmux page was last persisted, by its
   * durable key, until that session is on screen again. A runner restart
   * resumes it into a new tab, possibly after other tabs have arrived and the
   * reconcile has shown one of them for want of anything else; the moment the
   * session's new tab appears it is shown again. Cleared by any operator swap
   * (and by entering or leaving the mode), so it never takes the screen from a
   * session the operator chose.
   */
  const [pendingShownKey, setPendingShownKey] = useState<string | null>(() => {
    const shownBefore = persistedState?.assignments?.[0];
    if (initialMode.workMode !== "tmux" || !shownBefore) return null;
    return initialMode.mruSessionKeys[shownBefore] ?? null;
  });
  const [gridSnapshot, setGridSnapshot] = useState<GridSnapshot | null>(initialMode.gridSnapshot);
  const [layoutId, setLayoutIdState] = useState<string>(
    initialMode.workMode === "tmux" ? "single" : (persistedState?.layoutId ?? "single"),
  );
  const [assignments, setAssignments] = useState<ZoneAssignments>(
    persistedState?.assignments ?? {},
  );
  /** Who chose `layoutId`; `undefined` = a page persisted before the field. */
  const [layoutOrigin, setLayoutOrigin] = useState<LayoutOrigin | undefined>(() =>
    parseLayoutOrigin(persistedState?.layoutOrigin),
  );
  /**
   * The last layout switch or re-deal, committed with it: `seq` is bumped by
   * every `layoutId` change and by a grid compaction (which re-deals every
   * zone even when it keeps the id), `origin` is `auto` or `operator` as for
   * `layoutOrigin`, or `null` for one that is neither (a boot restore, a tmux
   * enter or toggle exit). `useZoneLabelsAndTags` keys on `seq` and carries
   * labels, notes and pins by tab when the origin is `auto`.
   */
  const [layoutSwitch, setLayoutSwitch] = useState<LayoutSwitch>(INITIAL_LAYOUT_SWITCH);
  const publishSwitch = useCallback((origin: LayoutOrigin | null) => {
    setLayoutSwitch((prev) => ({ seq: prev.seq + 1, origin }));
  }, []);
  /**
   * Has the page's current restore run settled? Cleared by
   * `useTerminalInitialization` when a run starts (the first one, or a drain
   * re-run after `initialized` is already true) and set when that run opens
   * its auto-save gate. Until then tab counts are the drain's, and an owed
   * auto-trim waits. True for a page no restore is running on.
   *
   * An ABORTED restore (an indeterminate reconnect) leaves it false, so the
   * trim stays held until the page's next restore run opens the gate,
   * exactly as auto-save stays closed until then.
   */
  const [restoreSettled, setRestoreSettled] = useState(true);
  const [focusedZone, setFocusedZone] = useState<number>(
    initialMode.workMode === "tmux" ? 0 : (persistedState?.focusedZone ?? 0),
  );
  /** Zone index that is temporarily maximized (null = normal grid) */
  const [maximizedZone, setMaximizedZone] = useState<number | null>(null);
  /**
   * The mode as of the last enter/exit — written synchronously by those two
   * transitions (the only writers of `workMode`), so a request made from an
   * async continuation (a spawn resolving after an `await`) reads the mode as
   * it is now, not as it was when its closure was created.
   */
  const workModeRef = useRef<TerminalWorkMode>(initialMode.workMode);
  /**
   * Liveness and session keys as of the last commit, for the transitions and
   * effects below that must read them without re-running whenever a tab's
   * liveness or session id moves (the tab list itself is what they key on).
   */
  const tabFactsRef = useRef({ liveTabIds, sessionKeyByTab });
  useEffect(() => {
    tabFactsRef.current = { liveTabIds, sessionKeyByTab };
  }, [liveTabIds, sessionKeyByTab]);
  const sessionKeyOf: SessionKeyOf = useCallback(
    (tabId: string) => tabFactsRef.current.sessionKeyByTab?.get(tabId),
    [],
  );
  /**
   * A one-shot "bring this tab into view and give it the keyboard" request
   * (a Session Manager card click). `seq` makes a repeat click on the same tab
   * a new request: focus moves alone cannot express it, because clicking the
   * already-focused session changes no focus state at all.
   */
  const [revealRequest, setRevealRequest] = useState<{ tabId: string; seq: number } | null>(null);
  const requestReveal = useCallback((tabId: string) => {
    setRevealRequest((prev) => ({ tabId, seq: (prev?.seq ?? 0) + 1 }));
  }, []);
  /**
   * Mark a reveal handled. The request is consumed rather than left standing
   * because the page provider swaps which page's layout is exposed: a request
   * left in place would replay — and steal keyboard focus — every time its
   * page became active again. Keeps `seq` so the next click is still new.
   */
  const consumeReveal = useCallback((seq: number) => {
    setRevealRequest((prev) => (prev && prev.seq === seq ? { ...prev, tabId: "" } : prev));
  }, []);

  // Zones explicitly claimed by a durable session record during restore.
  // The creation-order auto-fill below MUST NOT steal these even while the
  // record's tab hasn't landed in `tabIds` yet — otherwise a plain shell
  // created first grabs zone 0/3 and the real Claude session spills to an
  // unassigned slot (the creation-order bug this registry fix targets).
  // `assignTabToZone` registers the zone here so the guard holds across the
  // async restore window; the reservation is cleared once the zone is filled.
  const reservedZonesRef = useRef<Set<number>>(new Set());
  // Exposed as a getter, not the ref's `.current` value, so a caller (the
  // reveal-click handler) always reads it at CALL time — from an event
  // handler, never during render, where a ref read is stale-by-design.
  const getReservedZones = useCallback((): ReadonlySet<number> => reservedZonesRef.current, []);

  // Resolve through `resolveLayout` so a persisted/auto-grown `flow-grid` id
  // materializes a synthesized preset sized to the CURRENT live tabs, instead
  // of silently falling back to `single` and re-hiding every session past the
  // 9th. In flow mode that is one zone per live tab, stretched to reach the
  // highest assigned zone (`flowGridSlotCount`) so no assigned tab is ever
  // left without a zone. Presets ignore the count.
  const layout = resolveLayout(layoutId, flowGridSlotCount(tabIds, assignments));

  // The committed state, for the handlers below that must read it outside a
  // state updater: `applyLayout`'s focus clamp (which sizes the flow-grid the
  // same way), the tmux enter/exit transitions, and the reconcile effect's MRU
  // read. Handlers and effects run after the commit this effect mirrors.
  const committedRef = useRef({
    layoutId,
    assignments,
    focusedZone,
    maximizedZone,
    mru,
    gridSnapshot,
  });
  useEffect(() => {
    committedRef.current = { layoutId, assignments, focusedZone, maximizedZone, mru, gridSnapshot };
  }, [layoutId, assignments, focusedZone, maximizedZone, mru, gridSnapshot]);

  /**
   * The tabs whose terminals the grid has mounted, as `ZoneGrid` last reported
   * them (`reportMountedTabs`). Entering tmux keeps exactly these warm first
   * (`mruOnEnter`), so a switch from a scrolled flow grid unmounts nothing the
   * operator was looking at. `null` until a grid reports.
   */
  const mountedTabsRef = useRef<ReadonlySet<string> | null>(null);
  const reportMountedTabs = useCallback((tabIdsMounted: ReadonlySet<string>) => {
    mountedTabsRef.current = tabIdsMounted;
  }, []);

  // Persist on changes.
  useEffect(() => {
    persistState(pageId, windowLabel, {
      layoutId,
      assignments,
      focusedZone,
      workMode,
      mru,
      mruSessionKeys,
      gridSnapshot,
      // Written only once known: an absent origin must stay absent (legacy).
      ...(layoutOrigin ? { layoutOrigin } : {}),
    });
  }, [
    pageId,
    windowLabel,
    layoutId,
    assignments,
    focusedZone,
    workMode,
    mru,
    mruSessionKeys,
    gridSnapshot,
    layoutOrigin,
  ]);

  // Reconcile assignments with the live tabs. Grid: auto-assign tabs to empty
  // zones in creation order. tmux: keep zone 0 = the shown tab, and when it is
  // gone show the most recently shown survivor — never creation order first.
  // The MRU is read from the ref rather than keyed on: an MRU update must not
  // re-run this with a tab list that predates the tab it just recorded.
  useEffect(() => {
    setAssignments((prev) =>
      workMode === "tmux"
        ? reconcileTmuxAssignments(
            prev,
            tabIds,
            committedRef.current.mru,
            tabFactsRef.current.liveTabIds ?? null,
          )
        : reconcileAssignments(prev, tabIds, layout.zones.length, reservedZonesRef.current),
    );
  }, [tabIds, layout.zones.length, workMode]);

  // tmux mode: keep the grid snapshot, the MRU and the shown session pointing
  // at the tabs that exist now. A runner restart cold-resumes every session
  // into a NEW tab id, and a Claude session binds its id only after its tab
  // exists; re-keying by session id is what lets the snapshot's zones — and
  // the labels, notes and pins keyed by them — follow their sessions
  // (`rekeyGridSnapshot`), the MRU keep its order (`rekeyMru`), and the
  // session shown before the restart come back on screen (`pendingShownKey`).
  // Adjusted during render, like the MRU below, so none of it lags the tab list.
  const [rekeyedFor, setRekeyedFor] = useState<{
    tabIds: string[];
    keys: ReadonlyMap<string, string> | undefined;
    alive: ReadonlySet<string> | undefined;
    mru: string[];
  } | null>(null);
  if (
    workMode === "tmux" &&
    (rekeyedFor?.tabIds !== tabIds ||
      rekeyedFor?.keys !== sessionKeyByTab ||
      rekeyedFor?.alive !== liveTabIds ||
      rekeyedFor?.mru !== mru)
  ) {
    setRekeyedFor({ tabIds, keys: sessionKeyByTab, alive: liveTabIds, mru });
    const keyOf = (id: string) => sessionKeyByTab?.get(id);
    const alive = liveTabIds ?? null;
    if (gridSnapshot !== null) {
      const rekeyed = rekeyGridSnapshot(gridSnapshot, tabIds, keyOf, alive);
      if (rekeyed !== gridSnapshot) setGridSnapshot(rekeyed);
    }
    const rekeyedMru = rekeyMru(mru, mruSessionKeys, tabIds, keyOf, alive);
    if (rekeyedMru.mru !== mru) setMru(rekeyedMru.mru);
    if (rekeyedMru.keys !== mruSessionKeys) setMruSessionKeys(rekeyedMru.keys);
    if (pendingShownKey !== null) {
      const successor = tabsBySessionKey(tabIds, keyOf, alive).get(pendingShownKey);
      if (successor !== undefined) {
        setPendingShownKey(null);
        if (assignments[0] !== successor) setAssignments({ 0: successor });
      }
    }
  }

  // tmux mode's shown tab, and the ONE place the MRU is maintained: every swap
  // (a session-manager click, a cycler, a spawn, a close falling back, the UI
  // Bridge) is a change of zone 0, so recording it here covers them all.
  // Adjusted during render (React's "state from a changed value" pattern)
  // rather than in an effect, so the MRU never lags the screen by a commit.
  // An updater, so it lands on top of a re-key made in this same render. A
  // shown id naming no tab is not recorded: it is a tab a runner restart took
  // away, already in the MRU, and the re-key above (or the reconcile) is
  // about to show its successor — pushing it would put a dead entry at the
  // head that the re-key has already moved past. Nor is it marked recorded:
  // a tab assigned to zone 0 before it appears in `tabIds` is recorded on the
  // render it arrives.
  const shownTabId = workMode === "tmux" ? (assignments[0] ?? null) : null;
  const [recordedShownTabId, setRecordedShownTabId] = useState<string | null>(null);
  if (shownTabId !== recordedShownTabId && (shownTabId === null || tabIds.includes(shownTabId))) {
    setRecordedShownTabId(shownTabId);
    if (shownTabId !== null) {
      setMru((prev) => pushMru(prev, shownTabId));
    }
  }

  // Closing the maximized terminal returns to the grid. Otherwise the
  // maximized view keeps pointing at the zone the close emptied: a blank page
  // with no header. Only a CLOSE un-maximizes (the tab that was shown is gone
  // from `tabIds`). A tab dragged out, or an empty zone maximized on purpose
  // (the UI Bridge `maximize-zone` action allows it and reports the result),
  // is left alone.
  const maximizedTabRef = useRef<string | null>(null);
  useEffect(() => {
    const shown = maximizedZone === null ? null : (assignments[maximizedZone] ?? null);
    const previous = maximizedTabRef.current;
    maximizedTabRef.current = shown;
    if (shown === null && previous !== null && !tabIds.includes(previous)) {
      setMaximizedZone(null);
    }
  }, [assignments, maximizedZone, tabIds]);

  // Shared layout-application logic: switch the preset and redistribute
  // assignments. Used by the request-facing `setLayoutId` and the
  // programmatic auto-grow and auto-trim effects. `origin` is who chose the
  // layout; `null` (a boot restore) keeps the stored origin.
  const applyLayout = useCallback(
    (id: string, origin: LayoutOrigin | null) => {
      // `resolveLayout` accepts the synthetic `flow-grid` id (auto-grow calls
      // `applyLayout` directly, so a plain `LAYOUT_PRESETS.find` returning
      // undefined would EAT the grow into flow mode). Unknown ids resolve to
      // `single` rather than no-op — `setLayoutId` already gates bad input.
      // Flow mode is sized by `flowGridSlotCount`, exactly as the rendered
      // layout is. Sizing it by the bare tab count re-packed the survivors
      // whenever flow-grid was re-applied after a close (`/layout flow-grid`,
      // a profile load, a quick or AI launch): the highest tile counted as
      // out of range and was compacted into the hole, wearing that zone's
      // label, notes and pin.
      const zoneCountFor = (current: ZoneAssignments) =>
        resolveLayout(id, flowGridSlotCount(tabIds, current)).zones.length;
      const zoneCount = zoneCountFor(committedRef.current.assignments);

      // An operator request always records the operator's choice. An `auto`
      // one records only a layout it actually changed: re-asserting the
      // operator's own quad must not hand it to the trim.
      const changed = id !== committedRef.current.layoutId;
      setLayoutIdState(id);
      if (origin === "operator" || (origin === "auto" && changed)) setLayoutOrigin(origin);
      if (changed) publishSwitch(origin);
      setMaximizedZone(null);

      // Reassign: preserve every in-range assignment exactly; compact only
      // out-of-range ones; fill remaining zones with unassigned tabs. See
      // `applyLayoutAssignments` (item 10 — recorded zones must survive
      // layout application; dead-tab cleanup belongs to
      // `reconcileAssignments`, which runs with fresh tabIds).
      setAssignments((prev) => applyLayoutAssignments(prev, tabIds, zoneCountFor(prev)));

      // Clamp focused zone
      setFocusedZone((prev) => (prev >= zoneCount ? 0 : prev));
    },
    [tabIds, publishSwitch],
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
    const target = computeAutoGrowLayoutId(layoutId, tabIds.length, workMode);
    if (target) {
      applyLayout(target, "auto");
    }
  }, [layoutId, tabIds.length, workMode, applyLayout]);

  /**
   * Enter tmux mode: save the current grid as the snapshot to return to, pin
   * the layout to `single`, un-maximize, and show the tab the operator was
   * looking at — the maximized zone's when a zone is maximized (it is the only
   * one on screen), else the focused zone's — or the most recently shown live
   * tab, else the first live tab. No-op when already in tmux.
   */
  const enterTmuxMode = useCallback(() => {
    if (workModeRef.current === "tmux") return;
    workModeRef.current = "tmux";
    const committed = committedRef.current;
    const shown = shownTabOnEnter(
      committed.assignments,
      committed.maximizedZone ?? committed.focusedZone,
      committed.mru,
      tabIds,
      tabFactsRef.current.liveTabIds ?? null,
    );
    setGridSnapshot({
      layoutId: committed.layoutId,
      assignments: committed.assignments,
      focusedZone: committed.focusedZone,
      sessionKeys: sessionKeysFor(committed.assignments, sessionKeyOf),
    });
    setMru(
      mruOnEnter(
        shown,
        committed.assignments,
        committed.mru,
        tabIds,
        mountedTabsRef.current ?? undefined,
      ),
    );
    setPendingShownKey(null);
    setWorkMode("tmux");
    setLayoutIdState("single");
    publishSwitch(null);
    setMaximizedZone(null);
    setFocusedZone(0);
    setAssignments(shown === null ? {} : { 0: shown });
  }, [tabIds, sessionKeyOf, publishSwitch]);

  /**
   * Leave tmux mode for the grid the operator left: the snapshot, reconciled
   * against the tabs live now (`restoreGridSnapshot`). With no snapshot (a
   * page persisted in tmux by a build that kept none) the grid is
   * `pickLayout(tabCount)` with the shown tab kept in zone 0. `presetId` (an
   * operator layout pick made in tmux) is then applied OVER the restored
   * assignments, exactly as a pick in grid mode applies over the current ones.
   */
  const exitTmux = useCallback(
    (presetId: string | null) => {
      if (workModeRef.current !== "tmux") return;
      workModeRef.current = "grid";
      const committed = committedRef.current;
      const snapshot = committed.gridSnapshot;
      const restored: Omit<GridSnapshot, "sessionKeys"> = snapshot
        ? restoreGridSnapshot(
            snapshot,
            tabIds,
            resolveLayout(snapshot.layoutId, flowGridSlotCount(tabIds, snapshot.assignments)).zones
              .length,
            sessionKeyOf,
            tabFactsRef.current.liveTabIds ?? null,
          )
        : (() => {
            const fallbackId = pickLayout(tabIds.length);
            const count = resolveLayout(
              fallbackId,
              flowGridSlotCount(tabIds, committed.assignments),
            ).zones.length;
            return {
              layoutId: fallbackId,
              assignments: applyLayoutAssignments(committed.assignments, tabIds, count),
              focusedZone: 0,
            };
          })();
      let next = restored;
      if (presetId !== null) {
        const count = resolveLayout(presetId, flowGridSlotCount(tabIds, restored.assignments)).zones
          .length;
        next = {
          layoutId: presetId,
          assignments: applyLayoutAssignments(restored.assignments, tabIds, count),
          focusedZone: restored.focusedZone >= count ? 0 : restored.focusedZone,
        };
      }
      setWorkMode("grid");
      setGridSnapshot(null);
      setPendingShownKey(null);
      setLayoutIdState(next.layoutId);
      // A preset pick is an operator choice of layout. A toggle exit returns
      // to the grid as it was, with the origin it had.
      if (presetId !== null) setLayoutOrigin("operator");
      publishSwitch(presetId !== null ? "operator" : null);
      setAssignments(next.assignments);
      setFocusedZone(next.focusedZone);
      setMaximizedZone(null);
      // The keyboard goes where the restored focus ring is. Without this the
      // DOM focus stays in the last-shown tab's terminal — now one zone among
      // many, and not the highlighted one — so typing would land somewhere
      // other than where the page says it will.
      const focusedTab = next.assignments[next.focusedZone];
      if (focusedTab) requestReveal(focusedTab);
    },
    [tabIds, sessionKeyOf, requestReveal, publishSwitch],
  );

  const exitTmuxMode = useCallback(() => exitTmux(null), [exitTmux]);

  /**
   * Layout request, tagged with who made it. The choice is a STARTING POINT,
   * not a latch: if live sessions later overflow the chosen layout's capacity,
   * auto-grow still expands it — every live session must render in a zone.
   *
   *   - `operator` (picker, layout chords, `/layout`, profile/workspace load):
   *     applied in grid mode; in tmux mode it is an explicit request for a
   *     grid, so it exits tmux and applies the preset over the restored grid.
   *   - `auto` (a layout the runner chose: a quick or AI launch growing the
   *     grid) and `restore` (mount re-applying the saved preset): applied in
   *     grid mode; no-ops in tmux, which would otherwise end on every spawn
   *     and every reload.
   *
   * The source also records the layout's origin (`zoneAutoTrim.ts`), which
   * decides whether closing tabs may trim it: `operator` and `auto` record
   * themselves; `restore` keeps the stored origin, because stamping it would
   * turn every auto page into a never-trimmed one (or the reverse) on restart.
   */
  const setLayoutId = useCallback(
    (id: string, request: { source: LayoutRequestSource }) => {
      // Accept the synthetic flow-grid id alongside the static presets — a
      // persisted/auto-grown `flow-grid` must be re-applyable, else restoring
      // into flow mode silently drops back to a preset and re-hides sessions.
      if (id !== FLOW_GRID_ID && !LAYOUT_PRESETS.some((l) => l.id === id)) return;
      if (workModeRef.current === "tmux") {
        if (request.source === "operator") exitTmux(id);
        return;
      }
      applyLayout(id, request.source === "restore" ? null : request.source);
    },
    [applyLayout, exitTmux],
  );

  /**
   * A terminal the operator just created: in tmux mode it becomes the shown
   * tab (tmux `new-window`), the previous one moving down the MRU. A no-op in
   * grid mode, where the reconcile effect gives it a zone. Explicit rather
   * than inferred from `tabIds` growing, because tabs also arrive from restore
   * and from background ingest (gate continuations), and neither may take the
   * screen — or the keyboard — from the session the operator is typing in.
   */
  const showSpawnedTab = useCallback((tabId: string) => {
    if (workModeRef.current !== "tmux") return;
    setPendingShownKey(null);
    setAssignments((prev) => (prev[0] === tabId ? prev : { 0: tabId }));
  }, []);

  /**
   * A pane replaced in place by a new tab (restart-in-zone). The new tab takes
   * the old one's place everywhere a place is kept: its zone in the live
   * assignments (zone 0 in tmux), its zone in the grid snapshot tmux returns
   * to — the zone whose label, notes and pin are the pane's — and its MRU
   * recency. The old tab's session key goes with it. Identical to
   * `assignTabToZone(zone, newId)` in grid mode, where the snapshot is `null`.
   */
  const replaceTab = useCallback((oldId: string, newId: string) => {
    if (oldId === newId) return;
    // In tmux only zone 0 is on screen, so a restart is an operator choice of
    // what is shown: a session still pending from a runner restart must not
    // take the screen back from it.
    if (workModeRef.current === "tmux") setPendingShownKey(null);
    const newKey = tabFactsRef.current.sessionKeyByTab?.get(newId);
    setAssignments((prev) => replaceTabInAssignments(prev, oldId, newId));
    setGridSnapshot((prev) =>
      prev === null ? prev : replaceTabInGridSnapshot(prev, oldId, newId, newKey),
    );
    setMru((prev) => replaceTabInMru(prev, oldId, newId));
    setMruSessionKeys((prev) => {
      if (!(oldId in prev)) return prev;
      const next = { ...prev };
      delete next[oldId];
      return next;
    });
  }, []);

  /**
   * A pending "keep AI sessions" compaction (`PruneTerminalsButton`): compact
   * `order` into zones 0..n-1 of the smallest layout that fits, once none of
   * `closing` is in `tabIds` any more.
   *
   * It waits rather than applying at request time because the closes and the
   * compaction are separate state updates: applied first, the auto-grow effect
   * would see the not-yet-closed tabs overflow the smaller layout and grow it
   * straight back. Applied after, the layout is sized to the tabs that are
   * actually left, so nothing can be hidden by it.
   */
  const [pendingCompaction, setPendingCompaction] = useState<{
    order: string[];
    closing: string[];
  } | null>(null);

  const requestCompaction = useCallback((order: string[], closing: string[]) => {
    setPendingCompaction({ order, closing });
  }, []);

  useEffect(() => {
    if (!pendingCompaction) return;
    const timer = setTimeout(() => setPendingCompaction(null), COMPACTION_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [pendingCompaction]);

  useEffect(() => {
    if (!pendingCompaction) return;
    if (pendingCompaction.closing.some((id) => tabIds.includes(id))) return;
    setPendingCompaction(null);
    // Tabs spawned between the request and now are appended rather than
    // dropped — every live tab must still land in a zone.
    const live = new Set(tabIds);
    const ordered = pendingCompaction.order.filter((id) => live.has(id));
    const inOrder = new Set(ordered);
    for (const id of tabIds) if (!inOrder.has(id)) ordered.push(id);
    // Every zone index is about to be re-dealt, so a restore reservation would
    // pin a zone that no longer means what the record meant. A record whose tab
    // lands later still reclaims its zone through `assignTabToZone`.
    reservedZonesRef.current.clear();
    if (workModeRef.current === "tmux") {
      // One session on screen and nothing to compact there: the compaction is
      // of the grid tmux mode will return to, so it lands in the snapshot.
      const snapshot = committedRef.current.gridSnapshot;
      const snapshotFocused = snapshot ? snapshot.assignments[snapshot.focusedZone] : undefined;
      const compacted = compactAssignments(ordered);
      // The grid tmux returns to is now the runner's choice.
      setLayoutOrigin("auto");
      setGridSnapshot({
        layoutId: pickLayout(ordered.length),
        assignments: compacted,
        focusedZone: Math.max(0, snapshotFocused ? ordered.indexOf(snapshotFocused) : 0),
        sessionKeys: sessionKeysFor(compacted, sessionKeyOf),
      });
      return;
    }
    const focusedTab = committedRef.current.assignments[focusedZone];
    setLayoutIdState(pickLayout(ordered.length));
    setLayoutOrigin("auto");
    // A re-deal even when `pickLayout` keeps the id (6 tabs → 5 is six-pack).
    publishSwitch("auto");
    setAssignments(compactAssignments(ordered));
    setMaximizedZone(null);
    setFocusedZone(Math.max(0, focusedTab ? ordered.indexOf(focusedTab) : 0));
  }, [pendingCompaction, tabIds, focusedZone, sessionKeyOf, publishSwitch]);

  // ── Auto-trim: shrink a runner-chosen layout when tabs close ──────────────
  // A tab-count DECREASE latches a trim as owed; the owed trim applies once no
  // hold is active (`autoTrimHold`), from the count as it is THEN. The holds
  // are this effect's dependencies, so releasing one re-runs it: never a
  // timer, never "the next close". Mount, load and upgrade never trim, since
  // the count starts at the mount-time count and no decrease has been seen.
  // No thrash with auto-grow: the target is
  // `pickLayout(count)`, which fits the tabs, so neither reducer has anything
  // to do after the other.
  const prevTabCountRef = useRef(tabIds.length);
  const trimOwedRef = useRef(false);
  useEffect(() => {
    const tabCount = tabIds.length;
    if (tabCount < prevTabCountRef.current) trimOwedRef.current = true;
    prevTabCountRef.current = tabCount;
    if (!trimOwedRef.current) return;
    const hold = autoTrimHold({
      initialized,
      restoreSettled,
      compactionPending: pendingCompaction !== null,
      maximizedZone,
      workMode,
    });
    if (hold !== null) return;
    trimOwedRef.current = false;
    if (!readAutoTrimEnabled()) return;
    const target = computeAutoTrimLayoutId(
      layoutId,
      layoutOrigin,
      tabCount,
      layout.zones.length,
      workMode,
    );
    // A layout switch in reaction to committed state (a close, a hold
    // releasing), exactly like auto-grow above: it cannot be derived during
    // render, because the owed latch spans commits.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    if (target) applyLayout(target, "auto");
  }, [
    tabIds.length,
    initialized,
    restoreSettled,
    pendingCompaction,
    maximizedZone,
    workMode,
    layoutId,
    layoutOrigin,
    layout.zones.length,
    applyLayout,
  ]);

  const assignTabToZone = useCallback((zoneIndex: number, tabId: string) => {
    // tmux mode renders zone 0 only: assigning there is a swap (the MRU effect
    // records it); any other zone does not exist on screen, so the tab stays
    // off-screen and live rather than holding an index nothing renders.
    if (workModeRef.current === "tmux") {
      if (zoneIndex !== 0) return;
      // An operator swap: the session shown before a restart no longer
      // reclaims the screen when it resumes.
      setPendingShownKey(null);
    }
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

  /**
   * Restore placing a session at its RECORDED grid zone (the session-registry
   * restore and the cosmetic-snapshot plan tabs). In grid mode this is
   * `assignTabToZone`. In tmux mode the recorded zone is a zone of the grid
   * the operator will return to, not of the one viewport, so the placement
   * goes into the grid snapshot: a restore must never swap the shown session
   * (a reload in tmux would otherwise show whichever session the grid had in
   * zone 0), and the restored session must get back the grid zone its label,
   * notes and pin are keyed by. Without a snapshot there is no grid to place
   * into, and the tab simply stays off screen.
   */
  const placeRestoredTab = useCallback(
    (zoneIndex: number, tabId: string) => {
      if (workModeRef.current !== "tmux") {
        assignTabToZone(zoneIndex, tabId);
        return;
      }
      if (zoneIndex < 0) return;
      setGridSnapshot((prev) => (prev ? placeInGridSnapshot(prev, zoneIndex, tabId) : prev));
    },
    [assignTabToZone],
  );

  /**
   * Focus a zone. tmux mode has one zone on screen, so a request for any
   * other (a restore re-applying the grid's saved focus) names a zone that is
   * not there and is dropped rather than leaving the focus ring on nothing.
   */
  const setFocusedZoneInMode = useCallback((zone: number) => {
    if (workModeRef.current === "tmux" && zone !== 0) return;
    setFocusedZone(zone);
  }, []);

  /**
   * The grid of this page as the operator will see it in grid mode: the live
   * grid in grid mode, the snapshot it returns to in tmux mode. What persists
   * a grid zone — the session registry's recorded zone, the cosmetic session
   * layout — reads this, so tmux mode's one viewport never overwrites the
   * zones sessions are recorded in.
   */
  const gridView = useMemo(
    () =>
      workMode === "tmux"
        ? {
            layoutId: gridSnapshot?.layoutId ?? "single",
            assignments: gridSnapshot?.assignments ?? EMPTY_ASSIGNMENTS,
            focusedZone: gridSnapshot?.focusedZone ?? 0,
          }
        : { layoutId, assignments, focusedZone },
    [workMode, gridSnapshot, layoutId, assignments, focusedZone],
  );

  /**
   * The zone whose label, notes, pin, tags and filter a rendered zone shows
   * and edits. In grid mode that is the zone itself. In tmux mode the one
   * viewport (zone 0) shows the shown session's GRID zone, from the snapshot —
   * so each session keeps its own cosmetics on screen, and an edit lands on
   * the zone the session returns to. `null` when the shown session has no
   * grid zone (created during tmux): it has no cosmetics to show, and the
   * editors are hidden rather than writing into some other session's zone.
   */
  const shownGridZone =
    workMode === "tmux" ? gridZoneOfTab(gridSnapshot, assignments[0] ?? null) : null;
  const cosmeticZoneFor = useCallback(
    (zoneIdx: number): number | null => {
      if (workMode !== "tmux") return zoneIdx;
      return zoneIdx === 0 ? shownGridZone : null;
    },
    [workMode, shownGridZone],
  );

  const focusedTabId = assignments[focusedZone] ?? null;

  /**
   * Keyboard zone navigation (`Ctrl+Tab` / `/focus next|prev`). Walks EVERY
   * zone of the layout and lands only on OCCUPIED ones (a zone with a tab
   * assigned) — see {@link findNextZone}.
   *
   * These report whether focus moved, which is the only honest source for it:
   * `/focus next` cannot compute the answer for itself without copying this
   * walk, and a copied walk is exactly the kind of drift this pipeline keeps
   * rediscovering. A four-zone grid holding one session has nowhere to move
   * focus to (`changed: false`) — but that is decided by which zones are
   * occupied, NOT by `min(zones, tabs)`: assignments are not dense (a tab can
   * sit in zone 3 with zones 0-1 empty), and the old `min` cap cycled focus
   * between empty zones there while the occupied ones stayed unreachable.
   */
  const focusAdjacentZone = useCallback(
    (direction: 1 | -1): { changed: boolean } => {
      const next = findNextZone(
        layout.zones.length,
        focusedZone,
        assignments,
        () => true,
        direction,
      );
      if (next === null || next === focusedZone) return { changed: false };
      setFocusedZone(next);
      return { changed: true };
    },
    [layout.zones.length, focusedZone, assignments],
  );

  const focusNextZone = useCallback(() => focusAdjacentZone(1), [focusAdjacentZone]);
  const focusPrevZone = useCallback(() => focusAdjacentZone(-1), [focusAdjacentZone]);

  // Maximize is a grid-mode view: tmux mode already shows one session at full
  // size, so both setters are no-ops there and the exposed value is `null`.
  const toggleMaximize = useCallback((zoneIndex?: number) => {
    if (workModeRef.current === "tmux") return;
    setMaximizedZone((prev) => {
      const target = zoneIndex ?? 0;
      return prev === target ? null : target;
    });
  }, []);

  const setMaximizedZoneInMode = useCallback((zone: number | null) => {
    if (workModeRef.current === "tmux") return;
    setMaximizedZone(zone);
  }, []);

  const { unassignedTabIds, exitedUnassignedTabIds } = partitionUnassignedTabIds(
    tabIds,
    assignments,
    liveTabIds,
  );

  /** Is this a multi-zone layout? */
  const isMultiZone = layout.zones.length > 1;

  /**
   * THE state cycler: focus the next zone (after `focusedZone`, wrapping)
   * whose tab is in `state`, and un-maximize so the operator can see it.
   * Returns whether such a zone was found.
   *
   * Walks all `layout.zones.length` zones, skipping unassigned slots — never
   * `min(zones, tabs)`. Assignments are not dense, so that clamp made a pill
   * reading "2 need input · Tab to cycle" unable to reach tabs parked in
   * zones 2 and 3 of a four-zone grid, while the (then hand-copied, unclamped)
   * error cycler beside it reached them fine. ONE parameterized walk now
   * serves every pill; the named cyclers below are thin bindings.
   */
  const focusNextInState = useCallback(
    (sessionStates: Record<string, SessionState>, state: SessionState): boolean => {
      const next = findNextZone(
        layout.zones.length,
        focusedZone,
        assignments,
        (tabId) => sessionStates[tabId] === state,
      );
      if (next === null) return false;
      setFocusedZone(next);
      setMaximizedZone(null);
      return true;
    },
    [layout.zones.length, focusedZone, assignments],
  );

  /**
   * Focus the next "needs-input" zone; when none is waiting, fall back to the
   * next errored zone (both are "needs the operator"). Returns true if either
   * was found.
   */
  const focusNextNeedsInput = useCallback(
    (sessionStates: Record<string, SessionState>): boolean =>
      focusNextInState(sessionStates, "needs-input") || focusNextInState(sessionStates, "error"),
    [focusNextInState],
  );

  /** Focus the next errored zone (the status strip's "N errors" pill). */
  const focusNextError = useCallback(
    (sessionStates: Record<string, SessionState>): boolean =>
      focusNextInState(sessionStates, "error"),
    [focusNextInState],
  );

  return {
    layout,
    layoutId,
    layoutOrigin,
    layoutSwitch,
    setRestoreSettled,
    setLayoutId,
    assignments,
    assignTabToZone,
    placeRestoredTab,
    requestCompaction,
    focusedZone,
    setFocusedZone: setFocusedZoneInMode,
    focusedTabId,
    focusNextZone,
    focusPrevZone,
    maximizedZone: workMode === "tmux" ? null : maximizedZone,
    setMaximizedZone: setMaximizedZoneInMode,
    toggleMaximize,
    workMode,
    enterTmuxMode,
    exitTmuxMode,
    shownTabId,
    mru,
    showSpawnedTab,
    replaceTab,
    reportMountedTabs,
    gridView,
    cosmeticZoneFor,
    revealRequest,
    requestReveal,
    consumeReveal,
    getReservedZones,
    unassignedTabIds,
    exitedUnassignedTabIds,
    isMultiZone,
    focusNextInState,
    focusNextNeedsInput,
    focusNextError,
  };
}

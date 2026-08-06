/**
 * Module-level external store for the terminal page's HOT per-tab data.
 *
 * Why this exists (plan `2026-07-28-runner-many-sessions-performance` Phase 1,
 * root cause A1): `lastOutputLines`, `activityData`, `stateDurations` and the
 * file-lock maps used to live in `useSessionStateTracking` /
 * `useFileLockTracking` React state, get spread into the single
 * `TerminalSessionContext` value, and be republished through the lifted
 * provider on every change. One terminal output frame therefore produced a new
 * context value and re-rendered EVERY `useTerminalSession()` consumer — the
 * whole terminal page, ~60 times a second per streaming tab.
 *
 * The fix is the house pattern already used by
 * `components/terminal/commands/registry.ts`, `lib/authOverlayStore.ts` and
 * `useLiveClaudeSessionNames.ts`: a module store with **cached snapshots that
 * are invalidated on mutation**, consumed through `useSyncExternalStore`. No
 * store library is installed and none is needed.
 *
 * Two subscription granularities:
 *
 *   - **per field** (`subscribeField` + `getField`) — whole-map consumers
 *     (search bars, panels, command handlers). The map object identity is
 *     replaced only when something in it actually changed, so a consumer that
 *     re-reads a field it doesn't care about never re-renders.
 *   - **per tab** (`subscribeTab` + `getTabSlice`) — `ZoneCell` and friends.
 *     Only the listeners of the tabs whose data changed are notified, so one
 *     streaming pane cannot re-render its 39 idle neighbours.
 *
 * `getField` / `getTabSlice` MUST return a stable identity between mutations:
 * `useSyncExternalStore` re-invokes `getSnapshot` after every render and loops
 * forever if a fresh object comes back each time. Both do (the field maps are
 * replaced only on real change; tab slices are memoized and invalidated only
 * for the tabs that changed).
 *
 * State that is NOT hot (session states, stale tabs, callbacks) deliberately
 * stays in React state / the context value: it changes on operator-visible
 * transitions, not per output frame.
 */

import type {
  IncomingLongWaitSignal,
  IncomingYieldRequest,
  LockState,
} from "./useFileLockTracking";

/** The full set of hot per-tab maps, each keyed by `tab.id`. */
export interface HotMaps {
  /** Last ~20 rendered lines per tab (compact cards, search, exports). */
  lastOutputLines: Record<string, string[]>;
  /** Sparkline ring buffer (bytes per 2 s bucket) per tab. */
  activityData: Record<string, number[]>;
  /** Formatted "time in current state" per tab. */
  stateDurations: Record<string, string>;
  /** File-lock state per tab. */
  lockStates: Record<string, LockState>;
  /** Incoming yield requests, keyed by the HOLDER tab id. */
  pendingYieldRequests: Record<string, IncomingYieldRequest[]>;
  /** Incoming long-wait signals, keyed by the WAITER tab id. */
  pendingLongWaitSignals: Record<string, IncomingLongWaitSignal[]>;
}

export type HotField = keyof HotMaps;

/** Everything one tab's cell needs, in a single memoized object. */
export interface TabHotSlice {
  lastOutputLines: string[];
  activity: number[];
  stateDuration: string | undefined;
  lockState: LockState | undefined;
  yieldRequests: IncomingYieldRequest[];
  longWaitSignals: IncomingLongWaitSignal[];
}

type Listener = () => void;

// Shared empty defaults so a tab with no data yet still gets a stable identity
// (a fresh `[]` per call would defeat both memo and useSyncExternalStore).
const EMPTY_LINES: string[] = [];
const EMPTY_ACTIVITY: number[] = [];
const EMPTY_YIELD_REQUESTS: IncomingYieldRequest[] = [];
const EMPTY_LONG_WAIT_SIGNALS: IncomingLongWaitSignal[] = [];

/** Slice handed to consumers for an unknown / absent tab. */
export const EMPTY_TAB_SLICE: TabHotSlice = Object.freeze({
  lastOutputLines: EMPTY_LINES,
  activity: EMPTY_ACTIVITY,
  stateDuration: undefined,
  lockState: undefined,
  yieldRequests: EMPTY_YIELD_REQUESTS,
  longWaitSignals: EMPTY_LONG_WAIT_SIGNALS,
});

const HOT_FIELDS: HotField[] = [
  "lastOutputLines",
  "activityData",
  "stateDurations",
  "lockStates",
  "pendingYieldRequests",
  "pendingLongWaitSignals",
];

function emptyMaps(): HotMaps {
  return {
    lastOutputLines: {},
    activityData: {},
    stateDurations: {},
    lockStates: {},
    pendingYieldRequests: {},
    pendingLongWaitSignals: {},
  };
}

/**
 * Shallow-compare two records by value identity and return the keys that
 * differ (including keys added/removed). Empty result ⇒ nothing to publish.
 */
function changedKeys(
  prev: Record<string, unknown>,
  next: Record<string, unknown>,
): string[] {
  const changed: string[] = [];
  for (const key of Object.keys(next)) {
    if (prev[key] !== next[key]) changed.push(key);
  }
  for (const key of Object.keys(prev)) {
    if (!(key in next)) changed.push(key);
  }
  return changed;
}

/** One terminal page's hot data. */
export class TerminalHotStore {
  private maps: HotMaps = emptyMaps();
  private readonly fieldListeners = new Map<HotField, Set<Listener>>();
  private readonly tabListeners = new Map<string, Set<Listener>>();
  /** Memoized per-tab slices; an entry is deleted when that tab changes. */
  private readonly tabSlices = new Map<string, TabHotSlice>();

  // ── Reads ────────────────────────────────────────────────────────────────

  /**
   * The whole map for one field. Identity is stable until that field actually
   * changes, which is what makes it safe as a `useSyncExternalStore` snapshot.
   */
  getField<F extends HotField>(field: F): HotMaps[F] {
    return this.maps[field];
  }

  /** Convenience read used by callbacks that only need one tab's lines. */
  getLastOutputLines(tabId: string): string[] {
    return this.maps.lastOutputLines[tabId] ?? EMPTY_LINES;
  }

  /**
   * Memoized per-tab view. Rebuilt lazily after the tab's data changed, so
   * repeated calls between mutations return the identical object.
   */
  getTabSlice(tabId: string): TabHotSlice {
    const cached = this.tabSlices.get(tabId);
    if (cached) return cached;
    const slice: TabHotSlice = {
      lastOutputLines: this.maps.lastOutputLines[tabId] ?? EMPTY_LINES,
      activity: this.maps.activityData[tabId] ?? EMPTY_ACTIVITY,
      stateDuration: this.maps.stateDurations[tabId],
      lockState: this.maps.lockStates[tabId],
      yieldRequests: this.maps.pendingYieldRequests[tabId] ?? EMPTY_YIELD_REQUESTS,
      longWaitSignals: this.maps.pendingLongWaitSignals[tabId] ?? EMPTY_LONG_WAIT_SIGNALS,
    };
    this.tabSlices.set(tabId, slice);
    return slice;
  }

  // ── Subscriptions ────────────────────────────────────────────────────────

  subscribeField(field: HotField, listener: Listener): () => void {
    let set = this.fieldListeners.get(field);
    if (!set) {
      set = new Set();
      this.fieldListeners.set(field, set);
    }
    set.add(listener);
    return () => {
      set.delete(listener);
      if (set.size === 0) this.fieldListeners.delete(field);
    };
  }

  subscribeTab(tabId: string, listener: Listener): () => void {
    let set = this.tabListeners.get(tabId);
    if (!set) {
      set = new Set();
      this.tabListeners.set(tabId, set);
    }
    set.add(listener);
    return () => {
      set.delete(listener);
      if (set.size === 0) this.tabListeners.delete(tabId);
    };
  }

  // ── Writes ───────────────────────────────────────────────────────────────

  /**
   * Replace one field wholesale. No-ops (and notifies nobody) when every entry
   * is reference-identical to the current map — the "publish only what
   * changed" bailout the periodic sweeps rely on.
   *
   * @returns true when something was published.
   */
  setField<F extends HotField>(field: F, next: HotMaps[F]): boolean {
    const prev = this.maps[field] as Record<string, unknown>;
    const changed = changedKeys(prev, next as Record<string, unknown>);
    if (changed.length === 0) return false;
    this.maps[field] = next;
    this.publish(field, changed);
    return true;
  }

  /**
   * Functional update of one field, mirroring a `useState` reducer. Returning
   * the same object from `updater` is the no-change bailout.
   */
  updateField<F extends HotField>(
    field: F,
    updater: (prev: HotMaps[F]) => HotMaps[F],
  ): boolean {
    return this.setField(field, updater(this.maps[field]));
  }

  /** Publish one tab's rendered lines (the per-output-frame hot write). */
  setTabOutputLines(tabId: string, lines: string[]): void {
    const prev = this.maps.lastOutputLines;
    if (prev[tabId] === lines) return;
    this.maps.lastOutputLines = { ...prev, [tabId]: lines };
    this.publish("lastOutputLines", [tabId]);
  }

  /**
   * Drop every entry for tabs that no longer exist, across all fields.
   * Called from the tab-roster cleanup effect.
   */
  retainTabs(keep: Set<string>): void {
    // Field-erased view: every HotMaps value is a `Record<string, …>`, but TS
    // can't narrow `this.maps[field]` through the union key.
    const maps = this.maps as unknown as Record<HotField, Record<string, unknown>>;
    for (const field of HOT_FIELDS) {
      const current = maps[field];
      const dead = Object.keys(current).filter((id) => !keep.has(id));
      if (dead.length === 0) continue;
      const next = { ...current };
      for (const id of dead) delete next[id];
      maps[field] = next;
      this.publish(field, dead);
    }
    for (const tabId of [...this.tabSlices.keys()]) {
      if (!keep.has(tabId)) this.tabSlices.delete(tabId);
    }
  }

  /** Wipe everything (page removed / test teardown). Listeners are kept. */
  reset(): void {
    const nonEmpty = HOT_FIELDS.filter(
      (field) => Object.keys(this.maps[field]).length > 0,
    );
    if (nonEmpty.length === 0) return;
    const touched = new Set<string>();
    for (const field of nonEmpty) {
      for (const id of Object.keys(this.maps[field])) touched.add(id);
    }
    this.maps = emptyMaps();
    this.tabSlices.clear();
    for (const field of nonEmpty) {
      for (const listener of this.fieldListeners.get(field) ?? []) listener();
    }
    for (const tabId of touched) {
      for (const listener of this.tabListeners.get(tabId) ?? []) listener();
    }
  }

  // ── Internals ────────────────────────────────────────────────────────────

  /** Invalidate the affected tab slices, then notify field + tab listeners. */
  private publish(field: HotField, changedTabIds: string[]): void {
    for (const tabId of changedTabIds) this.tabSlices.delete(tabId);
    for (const listener of this.fieldListeners.get(field) ?? []) listener();
    for (const tabId of changedTabIds) {
      for (const listener of this.tabListeners.get(tabId) ?? []) listener();
    }
  }
}

const stores = new Map<string, TerminalHotStore>();

/**
 * The hot store for one terminal page. Page-scoped (not global) because each
 * `PageSessionScope` owns a disjoint tab roster and its own tracking hooks —
 * exactly the same ownership boundary the `terminal-output` tap uses.
 */
export function getTerminalHotStore(pageId: string): TerminalHotStore {
  let store = stores.get(pageId);
  if (!store) {
    store = new TerminalHotStore();
    stores.set(pageId, store);
  }
  return store;
}

/** Test-only reset of the page registry. */
export function __resetTerminalHotStoresForTest(): void {
  stores.clear();
}

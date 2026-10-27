/**
 * Terminal-page work modes — the pure state model behind `useZoneLayout`'s
 * `workMode` (plan `2026-10-11-runner-terminal-tmux-work-mode`, Phase 1).
 *
 *   - `grid` (the default): every live session gets a zone. Auto-grow,
 *     creation-order reconcile and every layout request enforce it.
 *   - `tmux`: exactly ONE session on screen. The layout is pinned to `single`,
 *     zone 0 holds the shown tab, and every other live tab keeps running
 *     off-screen. Swapping is an assignment to zone 0; the hook records each
 *     shown tab in an MRU list so closing the shown tab falls back to the one
 *     shown before it (tmux's "last window").
 *
 * Entering tmux saves the operator's grid as a {@link GridSnapshot}; exiting
 * restores it, reconciled against the tabs that are live by then
 * ({@link restoreGridSnapshot}), because zone labels, notes and pins are keyed
 * by zone index and a re-dealt grid would put them on the wrong sessions.
 *
 * Everything here is pure, so the tests assert the rules without React.
 */

import type { ZoneAssignments } from "./useZoneLayout";
import { MAX_WEBGL_PANES } from "./backends/webglContextLru";

export type TerminalWorkMode = "grid" | "tmux";

/**
 * Who asked for a layout change. In tmux mode only an `operator` request means
 * "give me a grid" (and exits the mode); `auto` (a layout the runner chose: a
 * spawn, quick or AI launch growing the grid to fit) and `restore` (mount
 * re-applying the saved preset) are system-initiated and are no-ops there, or
 * the mode would end on every spawn and every reload.
 *
 * In grid mode the source also records the layout's origin
 * (`zoneAutoTrim.ts` `LayoutOrigin`): `operator` and `auto` record themselves,
 * `restore` keeps the stored origin. Only an `auto` layout is trimmed when
 * tabs close.
 */
export type LayoutRequestSource = "operator" | "auto" | "restore";

/**
 * Read a persisted work mode. An ABSENT value is grid — the page predates the
 * mode, and grid is what the operator last saw on it. A value that is present
 * but not a known mode is also grid, flagged `malformed` so the caller logs it:
 * an unreadable preference never resolves to the less familiar mode.
 */
export function parseWorkMode(raw: unknown): { mode: TerminalWorkMode; malformed: boolean } {
  if (raw === undefined || raw === null) return { mode: "grid", malformed: false };
  if (raw === "grid" || raw === "tmux") return { mode: raw, malformed: false };
  return { mode: "grid", malformed: true };
}

/** Upper bound on the MRU list — far past any realistic count of live sessions. */
export const MRU_LIMIT = 64;

/**
 * Record `tabId` as the most recently shown tab: moved (or added) to the head,
 * any earlier occurrence removed, and the list cut to `limit` entries. Returns
 * `mru` itself when `tabId` is already the head, so a state setter sees no change.
 */
export function pushMru(
  mru: readonly string[],
  tabId: string,
  limit: number = MRU_LIMIT,
): string[] {
  if (mru[0] === tabId) return mru as string[];
  return [tabId, ...mru.filter((id) => id !== tabId)].slice(0, limit);
}

/** Read a persisted MRU list: an array of strings, anything else is empty. */
export function parseMru(raw: unknown): string[] {
  if (!Array.isArray(raw)) return [];
  return raw.filter((id): id is string => typeof id === "string").slice(0, MRU_LIMIT);
}

/**
 * Which tabs are alive. `null` means liveness is not threaded and every tab
 * counts as alive (the hook's callers that predate the tmux mode pass none).
 */
export type AliveTabs = ReadonlySet<string> | null;

/**
 * The tab to show once `closedId` is gone (or, with `closedId` null, whenever
 * zone 0 is empty). A LIVE tab always beats an exited tombstone: the most
 * recently shown live tab, else the first live tab in creation order
 * (`tabIds` order). Only when no live tab is left does an exited one qualify,
 * in the same MRU-then-creation order, so the operator still sees how the last
 * session ended rather than a blank viewport. `null` when no tab is left.
 * Never `closedId` itself, even if it is still in `tabIds` (the close and the
 * tab-list update can land in either order).
 */
export function nextShownAfterClose(
  mru: readonly string[],
  tabIds: readonly string[],
  closedId: string | null,
  alive: AliveTabs = null,
): string | null {
  const present = new Set(tabIds);
  if (closedId !== null) present.delete(closedId);
  const liveTab = (id: string) => alive === null || alive.has(id);
  for (const id of mru) if (present.has(id) && liveTab(id)) return id;
  for (const id of tabIds) if (present.has(id) && liveTab(id)) return id;
  for (const id of mru) if (present.has(id)) return id;
  for (const id of tabIds) if (present.has(id)) return id;
  return null;
}

/**
 * The tab shown when tmux mode is entered: the focused zone's tab if it is
 * still a tab (the operator was looking at it, exited or not), else the
 * live-first fallback of {@link nextShownAfterClose}.
 */
export function shownTabOnEnter(
  assignments: ZoneAssignments,
  focusedZone: number,
  mru: readonly string[],
  tabIds: readonly string[],
  alive: AliveTabs = null,
): string | null {
  const focused = assignments[focusedZone];
  if (focused && tabIds.includes(focused)) return focused;
  return nextShownAfterClose(mru, tabIds, null, alive);
}

/**
 * The MRU on entering tmux mode. The sessions that were on screen in the grid
 * a moment ago are the most recently seen ones, so they follow the shown tab
 * ahead of whatever an earlier tmux visit left in the list.
 *
 * Among them, the ones whose terminals were MOUNTED at entry (`mountedAtEntry`)
 * come first — in zone order, then any mounted unzoned tab in creation order —
 * and only then the remaining zones in zone order. In a preset grid every zoned
 * tab is mounted, so this is plain zone order. In a scrolled flow grid only
 * the zones near the viewport are mounted; seeding the MRU in bare zone order
 * would make zones 0..N the warm set ({@link tmuxMountedTabIds} keeps the MRU
 * head warm), so entering tmux would unmount the panes the operator was just
 * looking at and cold-mount ones scrolled far away. The ordering also makes
 * "close the shown session" fall back to a session the operator just had on
 * screen. `mountedAtEntry` omitted: plain zone order.
 */
export function mruOnEnter(
  shown: string | null,
  assignments: ZoneAssignments,
  mru: readonly string[],
  tabIds: readonly string[],
  mountedAtEntry?: ReadonlySet<string>,
): string[] {
  const present = new Set(tabIds);
  const zoned = Object.keys(assignments)
    .map(Number)
    .sort((a, b) => a - b)
    .map((z) => assignments[z]);
  const mounted = mountedAtEntry
    ? [
        ...zoned.filter((id) => mountedAtEntry.has(id)),
        ...tabIds.filter((id) => mountedAtEntry.has(id)),
      ]
    : [];
  const ordered = [...(shown === null ? [] : [shown]), ...mounted, ...zoned, ...mru];
  const out: string[] = [];
  for (const id of ordered) {
    if (present.has(id) && !out.includes(id)) out.push(id);
  }
  return out.slice(0, MRU_LIMIT);
}

/**
 * How many off-screen tabs tmux mode keeps MOUNTED (warm) besides the shown
 * one: the most recently shown ones. Every other off-screen tab is
 * `offscreen-unmounted` and mounts on reveal, rebuilding from the PTY's
 * scrollback ring. Without the bound, entering tmux from a flow grid of dozens
 * of sessions would mount an xterm parser for every one of them at once — the
 * exact OOM the flow grid's virtualization exists to prevent (incident #535).
 *
 * The shown pane plus the warm ones fill exactly `MAX_WEBGL_PANES`, the WebGL
 * context cap (`backends/webglContextLru.ts`): every mounted pane can hold a
 * context, and a larger bound would only add panes that run on Canvas anyway —
 * memory cost without the fast renderer. Within the bound a swap is a prop
 * flip; past it, a ring replay.
 */
export const TMUX_WARM_TAB_LIMIT = MAX_WEBGL_PANES - 1;

/** The tab facts the warm set reads: plan and worker tabs never mount a pane. */
export interface MountableTab {
  id: string;
  type?: string;
  sessionBacked?: boolean;
}

/**
 * Can this tab hold a mounted terminal pane off screen? A plan tab (rendered
 * by its viewer, no PTY) and a Conductor worker tab (`sessionBacked`, no PTY)
 * cannot — `classifyTabs` never parks them — so they never take a warm slot.
 * An exited PTY tab can: its pane still mounts, showing how the session ended.
 */
function holdsWarmPane(tab: MountableTab): boolean {
  return tab.type !== "plan" && !tab.sessionBacked;
}

/**
 * The tabs tmux mode keeps mounted: the shown tab plus the
 * {@link TMUX_WARM_TAB_LIMIT} most recently shown other tabs that still exist
 * and can hold a pane. A plan or worker tab in the MRU is skipped rather than
 * counted, so it never pushes a PTY tab out of the warm set while mounting
 * nothing itself.
 */
export function tmuxMountedTabIds(
  shownTabId: string | null,
  mru: readonly string[],
  tabs: readonly MountableTab[],
  limit: number = TMUX_WARM_TAB_LIMIT,
): Set<string> {
  const byId = new Map(tabs.map((t) => [t.id, t] as const));
  const mounted = new Set<string>();
  if (shownTabId !== null && byId.has(shownTabId)) mounted.add(shownTabId);
  let warm = 0;
  for (const id of mru) {
    if (warm >= limit) break;
    const tab = byId.get(id);
    if (id === shownTabId || tab === undefined || mounted.has(id) || !holdsWarmPane(tab)) continue;
    mounted.add(id);
    warm++;
  }
  return mounted;
}

/**
 * Reconcile tmux-mode assignments against the tabs. The result holds at most
 * zone 0: the shown tab is kept while it is still a tab (an exited shown tab
 * stays on screen as its tombstone — the operator sees how it ended); when it
 * is gone (or zone 0 is empty) the next shown tab comes from
 * {@link nextShownAfterClose} — live tabs first, the MRU before creation
 * order. Returns `prev` (same identity) when nothing changed.
 */
export function reconcileTmuxAssignments(
  prev: ZoneAssignments,
  tabIds: readonly string[],
  mru: readonly string[],
  alive: AliveTabs = null,
): ZoneAssignments {
  const shown = prev[0];
  const shownPresent = shown !== undefined && tabIds.includes(shown);
  const onlyZoneZero = Object.keys(prev).every((z) => z === "0");
  if (shownPresent && onlyZoneZero) return prev;
  const next = shownPresent ? shown : nextShownAfterClose(mru, tabIds, shown ?? null, alive);
  if (next === null) return Object.keys(prev).length === 0 ? prev : {};
  return { 0: next };
}

/**
 * tmux mode's cycler: the next live tab after `shownTabId` in `tabs` order
 * (`direction` 1) or before it (-1), wrapping, that satisfies `matches` — the
 * tab list counterpart of `findNextZone`, since in tmux mode only zone 0
 * exists and the off-screen tabs are in no zone at all. The shown tab itself
 * is never the answer: `null` means there is nowhere to swap to. With no shown
 * tab the walk starts before the first tab (or after the last, going back).
 */
export function stepShownTab(
  tabs: readonly { id: string; isAlive?: boolean }[],
  shownTabId: string | null,
  direction: 1 | -1 = 1,
  matches: (tabId: string) => boolean = () => true,
): string | null {
  const n = tabs.length;
  if (n === 0) return null;
  const found = shownTabId === null ? -1 : tabs.findIndex((t) => t.id === shownTabId);
  const start = found >= 0 ? found : direction === 1 ? -1 : n;
  for (let i = 1; i <= n; i++) {
    const tab = tabs[(((start + direction * i) % n) + n) % n];
    if (tab.id === shownTabId || tab.isAlive === false) continue;
    if (matches(tab.id)) return tab.id;
  }
  return null;
}

/**
 * The grid the operator left when entering tmux mode.
 *
 * `assignments` names tabs by TAB id, and a tab id is not durable: a runner
 * restart cold-resumes every session into a NEW tab. `sessionKeys` therefore
 * records, per zone, the durable identity of the session in it — its
 * `claudeSessionId`, the key the cold-resume path re-creates a tab from — so
 * the zone can be re-matched to the session's new tab
 * ({@link rekeyGridSnapshot}). A zone whose tab has no session id (a plain
 * shell) has no entry: a shell is not resumed, so there is nothing to match.
 */
export interface GridSnapshot {
  layoutId: string;
  assignments: ZoneAssignments;
  focusedZone: number;
  /** zone index → the `claudeSessionId` of the session in that zone. */
  sessionKeys: Record<number, string>;
}

/** A tab's durable session key (`claudeSessionId`), if it has one. */
export type SessionKeyOf = (tabId: string) => string | undefined;

/** Read a zone-index-keyed map of strings; `null` when any entry is malformed. */
function parseZoneStringMap(raw: unknown): Record<number, string> | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const out: Record<number, string> = {};
  for (const [zone, value] of Object.entries(raw as Record<string, unknown>)) {
    const z = Number(zone);
    if (!Number.isInteger(z) || z < 0 || typeof value !== "string") return null;
    out[z] = value;
  }
  return out;
}

/**
 * Read a persisted grid snapshot; anything not shaped like one is `null`. A
 * focused zone must be a non-negative integer — a fractional or negative one
 * names no zone, and restoring it would focus nothing. The layout id must be
 * one the page can lay out (`isKnownLayoutId`: a preset or the flow grid) — an
 * unknown one would restore as `single` and fold every other zone's session
 * out of the grid, so the snapshot is malformed rather than half-honoured. A
 * snapshot saved before `sessionKeys` existed reads with none (its zones still
 * match by tab id).
 */
export function parseGridSnapshot(
  raw: unknown,
  isKnownLayoutId: (layoutId: string) => boolean,
): GridSnapshot | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.layoutId !== "string" || !isKnownLayoutId(r.layoutId)) return null;
  if (!Number.isInteger(r.focusedZone) || (r.focusedZone as number) < 0) return null;
  const assignments = parseZoneStringMap(r.assignments);
  if (assignments === null) return null;
  const sessionKeys = r.sessionKeys === undefined ? {} : parseZoneStringMap(r.sessionKeys);
  if (sessionKeys === null) return null;
  return { layoutId: r.layoutId, assignments, focusedZone: r.focusedZone as number, sessionKeys };
}

/**
 * The tab each durable session key runs in now. When two tabs carry one key —
 * a session's exited tombstone beside the live tab it was resumed into — the
 * LIVE tab wins, so a re-key never points a zone at a dead pane while the
 * session is running next to it. Within the same liveness the first tab in
 * creation (`tabIds`) order wins.
 */
export function tabsBySessionKey(
  tabIds: readonly string[],
  sessionKeyOf: SessionKeyOf,
  alive: AliveTabs = null,
): Map<string, string> {
  const isLive = (id: string) => alive === null || alive.has(id);
  const byKey = new Map<string, string>();
  for (const id of tabIds) {
    const key = sessionKeyOf(id);
    if (!key) continue;
    const held = byKey.get(key);
    if (held === undefined || (!isLive(held) && isLive(id))) byKey.set(key, id);
  }
  return byKey;
}

/** The durable keys of the sessions in `assignments`, zone by zone. */
export function sessionKeysFor(
  assignments: ZoneAssignments,
  sessionKeyOf: SessionKeyOf,
): Record<number, string> {
  const keys: Record<number, string> = {};
  for (const [zone, tabId] of Object.entries(assignments)) {
    const key = sessionKeyOf(tabId);
    if (key) keys[Number(zone)] = key;
  }
  return keys;
}

/**
 * Bring a snapshot's tab ids up to date with the tabs that exist now:
 *
 *   - a zone whose tab still exists keeps it, and records that tab's session
 *     key if it has gained one since (a Claude session binds its id after the
 *     tab is created);
 *   - a zone whose tab is gone but whose recorded session now runs in another
 *     tab — the new tab a runner restart cold-resumed it into — is re-pointed
 *     at that tab, unless the tab already holds another zone of the snapshot;
 *   - a zone whose tab is gone with no such match is left as it is, so a
 *     session that resumes later can still claim it.
 *
 * The tab a session key resolves to is {@link tabsBySessionKey}'s: a live tab
 * over an exited one carrying the same key. Returns `snapshot` itself when
 * nothing changed.
 */
export function rekeyGridSnapshot(
  snapshot: GridSnapshot,
  tabIds: readonly string[],
  sessionKeyOf: SessionKeyOf,
  alive: AliveTabs = null,
): GridSnapshot {
  const present = new Set(tabIds);
  const tabByKey = tabsBySessionKey(tabIds, sessionKeyOf, alive);
  const held = new Set(Object.values(snapshot.assignments).filter((id) => present.has(id)));
  const assignments: ZoneAssignments = { ...snapshot.assignments };
  const sessionKeys: Record<number, string> = { ...snapshot.sessionKeys };
  let changed = false;
  for (const [zone, tabId] of Object.entries(snapshot.assignments)) {
    const z = Number(zone);
    if (present.has(tabId)) {
      const key = sessionKeyOf(tabId);
      if (key && sessionKeys[z] !== key) {
        sessionKeys[z] = key;
        changed = true;
      }
      continue;
    }
    const recorded = snapshot.sessionKeys[z];
    const successor = recorded ? tabByKey.get(recorded) : undefined;
    if (successor && !held.has(successor)) {
      assignments[z] = successor;
      held.add(successor);
      changed = true;
    }
  }
  return changed ? { ...snapshot, assignments, sessionKeys } : snapshot;
}

/**
 * Put `tabId` into zone `zone` of a snapshot (a restore placing a session at
 * its recorded grid zone while tmux mode is on). The tab leaves any other zone
 * it held; the zone's old session key goes with its old tab, and
 * {@link rekeyGridSnapshot} records the new tab's key once it has one.
 */
export function placeInGridSnapshot(
  snapshot: GridSnapshot,
  zone: number,
  tabId: string,
): GridSnapshot {
  if (snapshot.assignments[zone] === tabId) return snapshot;
  const assignments: ZoneAssignments = {};
  const sessionKeys: Record<number, string> = {};
  for (const [z, id] of Object.entries(snapshot.assignments)) {
    if (id === tabId) continue;
    assignments[Number(z)] = id;
    const key = snapshot.sessionKeys[Number(z)];
    if (key !== undefined) sessionKeys[Number(z)] = key;
  }
  assignments[zone] = tabId;
  delete sessionKeys[zone];
  return { ...snapshot, assignments, sessionKeys };
}

/**
 * The grid zone the snapshot holds `tabId` in, or `null` — the zone whose
 * label, notes and pin belong to that session while tmux mode shows it.
 */
export function gridZoneOfTab(snapshot: GridSnapshot | null, tabId: string | null): number | null {
  if (snapshot === null || tabId === null) return null;
  for (const [zone, id] of Object.entries(snapshot.assignments)) {
    if (id === tabId) return Number(zone);
  }
  return null;
}

/**
 * Reconcile a saved grid against the tabs live at exit time. `zoneCount` is
 * the snapshot layout's capacity (the caller resolves it, sizing a flow-grid
 * to reach every live tab).
 *
 *   - a tab still live keeps its recorded zone EXACTLY (its label, notes and
 *     pin are keyed by that index) — and so does a session whose tab was
 *     replaced (a runner restart cold-resumes into a new tab id), matched
 *     through its recorded session key ({@link rekeyGridSnapshot});
 *   - a tab closed during tmux leaves its zone empty — nothing re-packs;
 *   - a tab created during tmux fills the lowest empty zone, in creation
 *     order; one that does not fit stays unassigned, and the grid's auto-grow
 *     makes room for it;
 *   - the focused zone is kept when it is inside the layout, else zone 0.
 */
export function restoreGridSnapshot(
  saved: GridSnapshot,
  liveTabIds: readonly string[],
  zoneCount: number,
  sessionKeyOf: SessionKeyOf = () => undefined,
  alive: AliveTabs = null,
): GridSnapshot {
  const snapshot = rekeyGridSnapshot(saved, liveTabIds, sessionKeyOf, alive);
  const live = new Set(liveTabIds);
  const assignments: ZoneAssignments = {};
  const placed = new Set<string>();
  for (const [zone, tabId] of Object.entries(snapshot.assignments)) {
    const z = Number(zone);
    if (z < zoneCount && live.has(tabId) && !placed.has(tabId)) {
      assignments[z] = tabId;
      placed.add(tabId);
    }
  }
  const unplaced = liveTabIds.filter((id) => !placed.has(id));
  for (let z = 0; z < zoneCount && unplaced.length > 0; z++) {
    if (!(z in assignments)) assignments[z] = unplaced.shift()!;
  }
  return {
    layoutId: snapshot.layoutId,
    assignments,
    focusedZone: snapshot.focusedZone < zoneCount ? snapshot.focusedZone : 0,
    sessionKeys: sessionKeysFor(assignments, sessionKeyOf),
  };
}

/**
 * tmux mode's MRU across a runner restart. The MRU names tabs by TAB id, and a
 * restart cold-resumes every session into a new tab, so `keys` records the
 * durable session key of each MRU entry that has one (persisted beside the
 * MRU). This brings both up to date with the tabs that exist now, exactly as
 * {@link rekeyGridSnapshot} does for the grid:
 *
 *   - an entry whose tab still exists keeps it, and records that tab's key if
 *     it has gained one since;
 *   - an entry whose tab is gone but whose recorded session now runs in another
 *     tab ({@link tabsBySessionKey}: live before exited) is re-pointed at that
 *     tab IN PLACE, keeping its recency — unless the tab is already further up
 *     the list, where the newer entry stands;
 *   - an entry whose tab is gone with no such match stays, so a session that
 *     resumes later can still claim its place.
 *
 * `keys` comes back holding exactly the entries of the returned MRU that have
 * a key. Both are returned as given (same identity) when nothing changed.
 */
export function rekeyMru(
  mru: readonly string[],
  keys: Readonly<Record<string, string>>,
  tabIds: readonly string[],
  sessionKeyOf: SessionKeyOf,
  alive: AliveTabs = null,
): { mru: string[]; keys: Record<string, string> } {
  const present = new Set(tabIds);
  const tabByKey = tabsBySessionKey(tabIds, sessionKeyOf, alive);
  const nextMru: string[] = [];
  const nextKeys: Record<string, string> = {};
  for (const id of mru) {
    const recorded = keys[id];
    const successor =
      !present.has(id) && recorded !== undefined ? tabByKey.get(recorded) : undefined;
    const entry = successor ?? id;
    if (nextMru.includes(entry)) continue;
    nextMru.push(entry);
    const key = (present.has(entry) ? sessionKeyOf(entry) : undefined) ?? recorded;
    if (key !== undefined) nextKeys[entry] = key;
  }
  const mruSame = nextMru.length === mru.length && nextMru.every((id, i) => id === mru[i]);
  const keyIds = Object.keys(nextKeys);
  const keysSame =
    keyIds.length === Object.keys(keys).length && keyIds.every((id) => keys[id] === nextKeys[id]);
  return {
    mru: mruSame ? (mru as string[]) : nextMru,
    keys: keysSame ? (keys as Record<string, string>) : nextKeys,
  };
}

/**
 * A pane replaced in place (restart-in-zone: a fresh terminal takes over a
 * finished one's place). Every zone holding `oldId` now holds `newId`, and
 * `newId` leaves any OTHER zone the creation-order fill may already have given
 * it, so it is never on screen twice. Zones keep their indices — the labels,
 * notes and pins keyed by them stay with the pane. Returns `assignments`
 * itself when `oldId` is in no zone.
 */
export function replaceTabInAssignments(
  assignments: ZoneAssignments,
  oldId: string,
  newId: string,
): ZoneAssignments {
  if (!Object.values(assignments).includes(oldId)) return assignments;
  const next: ZoneAssignments = {};
  for (const [zone, id] of Object.entries(assignments)) {
    if (id === oldId) next[Number(zone)] = newId;
    else if (id !== newId) next[Number(zone)] = id;
  }
  return next;
}

/**
 * {@link replaceTabInAssignments} for the grid tmux mode returns to: the
 * replaced pane's grid zone now holds the new tab. The zone's recorded session
 * key was the OLD session's, which the replacement does not continue, so it is
 * replaced by the new tab's key (`newKey`) or dropped. Returns `snapshot`
 * itself when `oldId` is in no zone of it.
 */
export function replaceTabInGridSnapshot(
  snapshot: GridSnapshot,
  oldId: string,
  newId: string,
  newKey: string | undefined,
): GridSnapshot {
  const assignments = replaceTabInAssignments(snapshot.assignments, oldId, newId);
  if (assignments === snapshot.assignments) return snapshot;
  const sessionKeys: Record<number, string> = {};
  for (const [zone, id] of Object.entries(assignments)) {
    const z = Number(zone);
    const key = id === newId ? newKey : snapshot.sessionKeys[z];
    if (key !== undefined) sessionKeys[z] = key;
  }
  return { ...snapshot, assignments, sessionKeys };
}

/**
 * {@link replaceTabInAssignments} for the MRU: `newId` takes `oldId`'s place
 * (its recency), and any other occurrence of `newId` goes. Returns `mru`
 * itself when `oldId` is not in it.
 */
export function replaceTabInMru(mru: readonly string[], oldId: string, newId: string): string[] {
  if (!mru.includes(oldId)) return mru as string[];
  const out: string[] = [];
  for (const id of mru) {
    const entry = id === oldId ? newId : id;
    if (!out.includes(entry)) out.push(entry);
  }
  return out;
}

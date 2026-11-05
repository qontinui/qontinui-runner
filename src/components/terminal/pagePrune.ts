/**
 * Empty auto-created pages leave the strip, and the legacy roster is repaired
 * once (plan
 * `2026-11-05-runner-terminal-pages-collide-crowd-and-overfill-and-limit-stalled-sessions-never-resume`,
 * Phase 6, D2 and D3).
 *
 * Every helper here is pure. The hook (`useTerminalPages`) owns the effects:
 * the occupancy reads, the record-only removal, the Undo window and the
 * persisted version key.
 *
 * What may be pruned is deliberately narrow: a page the RUNNER made
 * (`origin: "auto"`), never one the operator made, and only once nothing can
 * still be on its way to it. A pruned page carries only an id, an
 * auto-generated name and an empty layout, and the strip offers Undo.
 */

import { DEFAULT_PAGE_NAME, nextAutoPageName, uniquePageName } from "./pageNaming";
import type { TerminalPageConfig, TerminalPageOrigin } from "./useTerminalPages";

/** A page must be at least this old (ms) before the auto-prune may remove it. */
export const PRUNE_MIN_AGE_MS = 300_000;

/** How long (ms) the strip offers Undo for an auto-prune. */
export const PRUNE_UNDO_MS = 15_000;

/** `instanceStorage` key recording which one-time roster repair has run. */
export const PAGES_VERSION_KEY = "qontinui-terminal-pages-version";

/** The roster version this build's repair produces. */
export const PAGES_VERSION = 2;

const DEFAULT_PAGE_ID = "default";

/** Names only the runner ever gave a page. */
const AUTO_NAMES = [/^Page \d+$/, /^Restored [0-9a-f]{8}$/];

/**
 * The origin of a page persisted before `origin` existed (D2 (a)). A
 * non-default page with no `defaultWorkingDir`, named `Page <n>`, "Terminal" or
 * `Restored <8 hex>` (runner#2569's `ensurePagesPresent`), is `auto`; every
 * other page is `operator`: a name or cwd the operator gave it is information.
 * A page that already carries `origin` keeps it.
 */
export function classifyLegacyOrigin(page: TerminalPageConfig): TerminalPageOrigin {
  if (page.origin) return page.origin;
  if (page.id === DEFAULT_PAGE_ID) return "operator";
  if (page.defaultWorkingDir) return "operator";
  if (page.name === DEFAULT_PAGE_NAME || AUTO_NAMES.some((re) => re.test(page.name))) {
    return "auto";
  }
  return "operator";
}

/**
 * `pages` with every missing `origin` filled in by {@link classifyLegacyOrigin}.
 * Returns the SAME reference when every page already has one.
 */
export function withClassifiedOrigins(pages: TerminalPageConfig[]): TerminalPageConfig[] {
  if (pages.every((p) => p.origin)) return pages;
  return pages.map((p) => (p.origin ? p : { ...p, origin: classifyLegacyOrigin(p) }));
}

/** What the auto-prune must never remove, at the moment it decides. */
export interface PruneGuards {
  activePageId: string;
  /** `pageId → windowLabel` for pages bound to a pop-out. */
  boundPages: Readonly<Record<string, string>>;
  now: number;
}

/**
 * Pure: the ids the auto-prune may remove, in roster order (D3). Each is all
 * of: `origin: "auto"`; not `default`; not active; not bound to a pop-out;
 * created at least {@link PRUNE_MIN_AGE_MS} ago; and absent from `occupied`
 * (no live terminal, no restorable record). The caller passes only a COMPLETE
 * occupancy read.
 *
 * At least one DOCKED page other than `default` always survives (pages bound
 * to a pop-out do not count: the strip cannot show them): if every one of them
 * is a candidate, the first is kept, so the strip never goes blank.
 */
export function prunablePageIds(
  pages: readonly TerminalPageConfig[],
  occupied: ReadonlySet<string>,
  guards: PruneGuards,
): string[] {
  const candidates = pages.filter(
    (p) =>
      p.origin === "auto" &&
      p.id !== DEFAULT_PAGE_ID &&
      p.id !== guards.activePageId &&
      !guards.boundPages[p.id] &&
      guards.now - p.createdAt >= PRUNE_MIN_AGE_MS &&
      !occupied.has(p.id),
  );
  const docked = pages.filter((p) => p.id !== DEFAULT_PAGE_ID && !guards.boundPages[p.id]).length;
  if (candidates.length > 0 && candidates.length === docked) candidates.shift();
  return candidates.map((p) => p.id);
}

/** A page the record-only removal took out, with what Undo needs to put it back. */
export interface RemovedPage {
  page: TerminalPageConfig;
  /** Its index in the roster at the moment it was removed. */
  index: number;
  /** Its raw `page:<id>:` storage entries, as removed. */
  entries: Array<[string, string]>;
}

/** One rename the repair made. */
export interface PageRename {
  id: string;
  from: string;
  to: string;
}

/**
 * Pure: re-insert `removed` (in the order they were removed) at their prior
 * indices, each as `origin: "operator"` so the next reconcile does not prune
 * it again. Walking the removals backwards against their captured indices
 * restores the original order exactly. A restored page whose name is now taken
 * (or is "Terminal") is given a unique one, so Undo cannot bring a duplicate
 * back. A page already present is left alone.
 */
export function restoreRemovedPages(
  pages: TerminalPageConfig[],
  removed: readonly RemovedPage[],
): TerminalPageConfig[] {
  const out = pages.slice();
  for (let i = removed.length - 1; i >= 0; i--) {
    const { page, index } = removed[i];
    if (out.some((p) => p.id === page.id)) continue;
    const restored: TerminalPageConfig = {
      ...page,
      name: uniquePageName(page.name, out),
      origin: "operator",
    };
    out.splice(Math.min(index, out.length), 0, restored);
  }
  return out;
}

/**
 * Pure: the one-time name repair (D2 (b)). A non-default page named "Terminal"
 * takes {@link nextAutoPageName}. Among pages sharing a name ONE keeps it, in
 * this order of claim: an OCCUPIED page (the name the operator has been
 * reading on a populated tab), then an operator page, then the oldest
 * (`createdAt`, then roster order). Every other holder is renamed: a runner
 * name (`Page N`) takes {@link nextAutoPageName}, any other name takes
 * {@link uniquePageName}. Ids never change. Returns the renames in the order
 * they were made; an empty list means the roster was already clean.
 */
export function duplicateNameRepairs(
  pages: readonly TerminalPageConfig[],
  occupied: ReadonlySet<string>,
): PageRename[] {
  const roster = pages.map((p) => ({ ...p }));
  const renames: PageRename[] = [];
  const rename = (p: TerminalPageConfig, to: string) => {
    renames.push({ id: p.id, from: p.name, to });
    p.name = to;
  };

  for (const p of roster) {
    if (p.id !== DEFAULT_PAGE_ID && p.name === DEFAULT_PAGE_NAME) {
      rename(p, nextAutoPageName(roster));
    }
  }

  const claim = (p: TerminalPageConfig) =>
    (occupied.has(p.id) ? 0 : 2) + (p.origin === "operator" ? 0 : 1);
  const byClaim = roster
    .map((p, i) => ({ p, i }))
    .sort((a, b) => claim(a.p) - claim(b.p) || a.p.createdAt - b.p.createdAt || a.i - b.i)
    .map(({ p }) => p);
  const kept = new Set<string>();
  for (const p of byClaim) {
    if (!kept.has(p.name)) {
      kept.add(p.name);
      continue;
    }
    const to = AUTO_NAMES[0].test(p.name)
      ? nextAutoPageName(roster)
      : uniquePageName(p.name, roster);
    rename(p, to);
    kept.add(to);
  }
  return renames;
}

/** Pure: apply `renames` to `pages` by id. Same reference when there are none. */
export function applyRenames(
  pages: TerminalPageConfig[],
  renames: readonly PageRename[],
): TerminalPageConfig[] {
  if (renames.length === 0) return pages;
  const to = new Map(renames.map((r) => [r.id, r.to]));
  return pages.map((p) => (to.has(p.id) ? { ...p, name: to.get(p.id)! } : p));
}

/** An occupancy read and whether it can be trusted as the whole truth. */
export interface OccupancyRead {
  ids: Set<string>;
  /**
   * `false` when a source could not be read, or `terminal_session_list_open`
   * answered with `deferredByDrain` (the restorable set is WITHHELD, not
   * empty). Only a complete read may decide that a page is empty.
   */
  complete: boolean;
}

/** What {@link removeEach} removed, and whether every re-read it made was complete. */
export interface RemovalPass<T> {
  removed: T[];
  allReadsComplete: boolean;
  /** The ids of the last complete re-read, or `null` when none was made. */
  lastIds: Set<string> | null;
}

/**
 * The removal loop shared by "Close empty tabs" and the auto-prune. Occupancy
 * is RE-READ before each removal: a terminal may have landed on a candidate
 * while earlier pages were being removed. A re-read that is incomplete, or
 * that now lists the page, keeps it; an incomplete one is reported, so the
 * caller can tell a whole pass from a partial one.
 */
export async function removeEach<T>(
  candidates: readonly string[],
  read: () => Promise<OccupancyRead>,
  remove: (id: string) => Promise<T | null>,
): Promise<RemovalPass<T>> {
  const removed: T[] = [];
  let allReadsComplete = true;
  let lastIds: Set<string> | null = null;
  for (const id of candidates) {
    const fresh = await read();
    if (!fresh.complete) allReadsComplete = false;
    else lastIds = fresh.ids;
    if (!fresh.complete || fresh.ids.has(id)) continue;
    const r = await remove(id);
    if (r !== null) removed.push(r);
  }
  return { removed, allReadsComplete, lastIds };
}

/** The strip's notice text for an auto-prune and/or repair. */
export function pruneNoticeText(removedCount: number, renames: readonly PageRename[]): string {
  const parts: string[] = [];
  if (removedCount > 0) {
    parts.push(`Removed ${removedCount} empty auto-created page${removedCount === 1 ? "" : "s"}`);
  }
  if (renames.length > 0) {
    parts.push(`Renamed ${renames.map((r) => `"${r.from}" → "${r.to}"`).join(", ")}`);
  }
  return parts.join(" · ");
}

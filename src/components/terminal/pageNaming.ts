/**
 * The one page-name rule for the terminal tab strip (plan
 * `2026-11-05-runner-terminal-pages-collide-crowd-and-overfill-and-limit-stalled-sessions-never-resume`,
 * D1).
 *
 * A page name is a label, and a label names ONE referent: the reorganize maps
 * its target pages by name (`App.tsx` `pageNameToId`), and the operator picks a
 * tab by reading it. Before this module three callers each made up a name from
 * a COUNT (`Page ${nonDefaultCount + 1}` in `reconcilePages`,
 * `Page ${pages.length + 1}` over the visible pages in the tab bar's "+"), and a
 * count is not a name: with 18 non-default pages persisted the next page was
 * "Page 19" while "Page 20" through "Page 23" already existed.
 *
 * Every helper here is pure and takes the roster to check against. Callers must
 * pass the FULL roster — including pages detached into a pop-out — because a
 * name only has to be unique across every page the operator owns, not across
 * the tabs one window happens to show.
 */

/** The name reserved for the internal `default` page. No other page may use it. */
export const DEFAULT_PAGE_NAME = "Terminal";

/** The id of the internal default page (the API-terminal home). */
const DEFAULT_PAGE_ID = "default";

/** The inline refusal a rename to a taken name gets. */
export const NAME_IN_USE = "Name in use";

const AUTO_NAME = /^Page (\d+)$/;

/**
 * `Page <max N + 1>` over every name of the form `Page <N>`, or `Page 1` when
 * there is none. By construction the result can never already be present: a
 * page named `Page <max + 1>` would itself have been the max.
 */
export function nextAutoPageName(pages: ReadonlyArray<{ name: string }>): string {
  let max = 0;
  for (const p of pages) {
    const m = AUTO_NAME.exec(p.name);
    if (!m) continue;
    const n = Number(m[1]);
    if (Number.isSafeInteger(n) && n > max) max = n;
  }
  return `Page ${max + 1}`;
}

/**
 * `desired` if no page uses it, else `<desired> 2`, `<desired> 3`, … — the first
 * one free. "Terminal" is reserved for the default page, so asking for it (or
 * for a blank name) yields {@link nextAutoPageName} instead. Never call this for
 * the default page itself; it keeps "Terminal".
 */
export function uniquePageName(desired: string, pages: ReadonlyArray<{ name: string }>): string {
  const base = desired.trim();
  if (!base || base === DEFAULT_PAGE_NAME) return nextAutoPageName(pages);
  const used = new Set(pages.map((p) => p.name));
  if (!used.has(base)) return base;
  for (let k = 2; ; k++) {
    const candidate = `${base} ${k}`;
    if (!used.has(candidate)) return candidate;
  }
}

/**
 * Why renaming page `id` to `name` is refused, or `null` when it is allowed.
 * Refused: a name another page already carries, and "Terminal" for any page but
 * the default one. Keeping a page's own name is always allowed.
 */
export function renameRefusal(
  pages: ReadonlyArray<{ id: string; name: string }>,
  id: string,
  name: string,
): string | null {
  const next = name.trim();
  if (next === DEFAULT_PAGE_NAME && id !== DEFAULT_PAGE_ID) return NAME_IN_USE;
  return pages.some((p) => p.id !== id && p.name === next) ? NAME_IN_USE : null;
}

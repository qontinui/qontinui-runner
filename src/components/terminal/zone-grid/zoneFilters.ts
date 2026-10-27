/**
 * Per-session output filters for the zone grid (`ZoneGrid`).
 *
 * A filter belongs to a SESSION, not to a zone index, so it is stored by tab
 * id and the per-zone view is derived at render from the grid's assignments.
 * Whatever moves a tab (a swap, a drag, a close, a grow, an auto-trim, a
 * compaction, an operator's switch to a smaller layout) cannot strand its
 * filter on another tile, and a tab left without a zone gets its filter back
 * the moment it has one again. Pure, so it is tested without a DOM.
 */

/** Output filter text by tab id. */
export type TabFilters = Readonly<Record<string, string>>;

/** Set `tabId`'s filter to `value` (an empty string is kept: the input is open). */
export function setTabFilter(filters: TabFilters, tabId: string, value: string): TabFilters {
  if (filters[tabId] === value) return filters;
  return { ...filters, [tabId]: value };
}

/** Remove `tabId`'s filter. */
export function clearTabFilter(filters: TabFilters, tabId: string): TabFilters {
  if (!(tabId in filters)) return filters;
  const next = { ...filters };
  delete next[tabId];
  return next;
}

/** The filter input open on `tabId`, toggled. */
export function toggleFilterInputTab(open: string | null, tabId: string): string | null {
  return open === tabId ? null : tabId;
}

/** The per-zone filters the cells render: each zone shows its tab's filter. */
export function zoneFiltersFor(
  filters: TabFilters,
  assignments: Readonly<Record<number, string>>,
): Record<number, string> {
  const out: Record<number, string> = {};
  for (const [zone, tabId] of Object.entries(assignments)) {
    if (tabId && tabId in filters) out[Number(zone)] = filters[tabId];
  }
  return out;
}

/** The zone `tabId` sits in, or `null` (also for a `null` tab). */
export function zoneOfTab(
  assignments: Readonly<Record<number, string>>,
  tabId: string | null,
): number | null {
  if (tabId === null) return null;
  for (const [zone, id] of Object.entries(assignments)) {
    if (id === tabId) return Number(zone);
  }
  return null;
}

/**
 * Drop the filters (and the open filter input) of tabs no longer live, so a
 * closed session's entry does not outlive it. Same identity when nothing was
 * dropped.
 */
export function pruneTabFilters(
  filters: TabFilters,
  openInput: string | null,
  live: ReadonlySet<string>,
): { filters: TabFilters; openInput: string | null } {
  let next: Record<string, string> | null = null;
  for (const tabId of Object.keys(filters)) {
    if (live.has(tabId)) continue;
    next ??= { ...filters };
    delete next[tabId];
  }
  return {
    filters: next ?? filters,
    openInput: openInput !== null && !live.has(openInput) ? null : openInput,
  };
}

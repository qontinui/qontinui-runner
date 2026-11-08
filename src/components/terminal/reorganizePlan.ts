/**
 * Normalization for a reorganize proposal before it is applied (follow-up to
 * plan `2026-11-05-runner-terminal-pages-collide-crowd-and-overfill-and-limit-stalled-sessions-never-resume`
 * Phase 5).
 *
 * The apply handler routes each group to an existing page by EXACT name and
 * mints a page (through `addPage` -> `uniquePageName`, which trims) only when
 * no page matches. A group name the operator edited to `"Foo "` therefore
 * missed the existing "Foo", and `addPage` minted "Foo 2" for it. The existing
 * page then looked unassigned, so the reorganize's empty-page sweep removed it.
 * Trimming here makes the lookup compare the same string `uniquePageName`
 * would store. Two groups that become the same name after trimming route to
 * one page, which is the handler's existing behaviour for identical names.
 */
export interface ReorganizeGroup {
  name: string;
  sessionIds: string[];
}

export function normalizeReorganizeGroups<G extends ReorganizeGroup>(
  groups: ReadonlyArray<G>,
): G[] {
  return groups.map((g) => ({ ...g, name: g.name.trim() }));
}

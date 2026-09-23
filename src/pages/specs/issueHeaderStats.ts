/**
 * Header stats for `GlobalIssuesPanel`, labelled by the population they count.
 *
 * THE DEFECT: the header badges were computed from `issues` while the body
 * rendered `filteredIssues`. `issues` is the SERVER-filtered set (status /
 * category / severity, with status seeded `"active"` at mount) and
 * `filteredIssues` narrows it further by the search box — so the header and the
 * list diverged on the first keystroke, and the bare total badge was an
 * active-issue count wearing a "total" label from mount, with no search at all.
 *
 * The rule here: every number in the header describes the list directly below
 * it, and says which population that is. The headline is the SHOWN count; while
 * a search is narrowing the list the pre-search count is disclosed beside it
 * (`3 of 12 active`) rather than silently replaced — the same "filtered from N"
 * disclosure `ErrorMonitorTab` settled on, so the two issue surfaces answer the
 * same question the same way.
 */

export type IssueStatusFilter = "all" | "active" | "resolved" | "monitoring";

/**
 * The server-side filters a set of issues was FETCHED with. The header labels
 * by these, not by the live filter controls: after a filter change the loaded
 * rows still belong to the previous filter until the refetch lands (or
 * indefinitely, if it fails), and labelling them with the new filter would
 * mis-state their population.
 */
export interface LoadedIssueFilters {
  status: IssueStatusFilter;
  category: string;
  severity: string;
}

export interface IssueHeaderFilters extends LoadedIssueFilters {
  searchQuery: string;
}

interface IssueLike {
  severity: string;
  status: string;
}

export interface IssueHeaderStats {
  /** Issues in the list below (after server filters AND search). */
  shownCount: number;
  /** Issues the server returned for the current status/category/severity filters, before search. */
  fetchedCount: number;
  /** True while the search box is narrowing `fetchedCount` down to `shownCount`. */
  searchActive: boolean;
  /** Headline badge text, naming its population — e.g. `12 active`, `3 of 12 active`, `40 total`. */
  badgeText: string;
  /** Hover text spelling out every filter the headline count is subject to. */
  badgeTitle: string;
  /** ACTIVE critical issues in the list below. */
  critical: number;
  /** ACTIVE high-severity issues in the list below. */
  high: number;
}

function statusPopulationLabel(status: IssueStatusFilter): string {
  return status === "all" ? "total" : status;
}

/**
 * Compute the header stats from the fetched set and the shown (searched) set.
 * Pure so the labelling rule is testable in the runner's node-env vitest.
 */
export function computeIssueHeaderStats(
  fetched: readonly IssueLike[],
  shown: readonly IssueLike[],
  /** `status`/`category`/`severity` must be the filters `fetched` was loaded
   *  with (see {@link LoadedIssueFilters}); `searchQuery` is the live box. */
  filters: IssueHeaderFilters,
): IssueHeaderStats {
  const searchActive = filters.searchQuery.trim().length > 0;
  const label = statusPopulationLabel(filters.status);
  const badgeText = searchActive
    ? `${shown.length} of ${fetched.length} ${label}`
    : `${fetched.length} ${label}`;

  const narrowing: string[] = [
    filters.status === "all" ? "every status" : `status = ${filters.status}`,
  ];
  if (filters.category !== "all") narrowing.push(`category = ${filters.category}`);
  if (filters.severity !== "all") narrowing.push(`severity = ${filters.severity}`);
  const badgeTitle = searchActive
    ? `${shown.length} issue(s) match the search, out of ${fetched.length} with ${narrowing.join(", ")}`
    : `${fetched.length} issue(s) with ${narrowing.join(", ")}`;

  let critical = 0;
  let high = 0;
  for (const issue of shown) {
    if (issue.status !== "active") continue;
    if (issue.severity === "critical") critical++;
    else if (issue.severity === "high") high++;
  }

  return {
    shownCount: shown.length,
    fetchedCount: fetched.length,
    searchActive,
    badgeText,
    badgeTitle,
    critical,
    high,
  };
}

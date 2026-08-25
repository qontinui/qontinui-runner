import { describe, it, expect } from "vitest";
import type { ErrorSeverity, ErrorStatus } from "../../types/errorMonitor";
import {
  DEFAULT_SELECTED_ERROR_STATUSES,
  ERROR_SEVERITY_FILTER_OPTIONS,
  ERROR_STATUS_FILTER_OPTIONS,
  UNRESOLVED_ERROR_STATUSES,
  disclosedCorpusTotal,
  filterBadgeCount,
} from "./errorFilterOptions";

/**
 * The `status IN (...)` list from `get_error_summary`'s `unresolved_count`
 * FILTER clause (`src-tauri/src/database/pg/error_monitor.rs`), transcribed by
 * hand. Deliberately NOT imported from the module under test: the whole point
 * is to compare the UI's default against the SQL's definition, and importing
 * the UI's own constant would compare it against itself.
 */
const SQL_UNRESOLVED_STATUSES: ErrorStatus[] = [
  "new",
  "recurring",
  "acknowledged",
  "in_progress",
  "promoted",
];

/**
 * Every member of `ErrorStatus`, spelled out. Deliberately NOT derived from the
 * module under test — that would make the exhaustiveness assertion circular. The
 * `Record` annotation is what keeps this list honest: adding a member to
 * `ErrorStatus` without adding it here fails to compile.
 */
const ALL_STATUSES = Object.keys({
  new: 0,
  acknowledged: 0,
  in_progress: 0,
  resolved: 0,
  ignored: 0,
  recurring: 0,
  promoted: 0,
} satisfies Record<ErrorStatus, number>) as ErrorStatus[];

const ALL_SEVERITIES = Object.keys({
  critical: 0,
  error: 0,
  warning: 0,
  info: 0,
  debug: 0,
} satisfies Record<ErrorSeverity, number>) as ErrorSeverity[];

describe("error monitor filter options", () => {
  /**
   * The defect, stated directly: the panel omitted a pill for `promoted`, so a
   * status could be selected — it was, by default — with nothing on screen to
   * show or clear it.
   */
  it("renders a pill for EVERY ErrorStatus, promoted included", () => {
    expect(ERROR_STATUS_FILTER_OPTIONS).toContain("promoted");
    for (const status of ALL_STATUSES) {
      expect(ERROR_STATUS_FILTER_OPTIONS).toContain(status);
    }
    expect(ERROR_STATUS_FILTER_OPTIONS).toHaveLength(ALL_STATUSES.length);
  });

  it("renders a pill for every ErrorSeverity", () => {
    for (const severity of ALL_SEVERITIES) {
      expect(ERROR_SEVERITY_FILTER_OPTIONS).toContain(severity);
    }
    expect(ERROR_SEVERITY_FILTER_OPTIONS).toHaveLength(ALL_SEVERITIES.length);
  });

  /**
   * The observable symptom: a FRESH tab, no interaction — badge read 4, at most
   * 3 pills could light.
   */
  it("badge count equals lit pill count on a fresh tab", () => {
    const litStatusPills = ERROR_STATUS_FILTER_OPTIONS.filter((s) =>
      DEFAULT_SELECTED_ERROR_STATUSES.includes(s),
    ).length;
    const litSeverityPills = ERROR_SEVERITY_FILTER_OPTIONS.filter(() => false).length;

    expect(litStatusPills).toBe(DEFAULT_SELECTED_ERROR_STATUSES.length);
    expect(filterBadgeCount([], [...DEFAULT_SELECTED_ERROR_STATUSES])).toBe(
      litStatusPills + litSeverityPills,
    );
    // 5 since iter 19 added `recurring` to the default set (was 4).
    expect(filterBadgeCount([], [...DEFAULT_SELECTED_ERROR_STATUSES])).toBe(5);
  });

  /**
   * Item A. The header renders `summary.unresolvedCount`; the body renders the
   * rows the default status filter admits. If those two sets differ, the tab
   * contradicts itself — measured at 6 unresolved in the header above 2 rows in
   * the body, the 4 missing ones all `recurring`.
   *
   * Asserted as SET EQUALITY in both directions. A subset check would pass a
   * default that hides rows (the actual defect); a superset check would pass a
   * default that admits `resolved`/`ignored` rows the header does not count.
   */
  it("the default status filter admits exactly what the summary calls unresolved", () => {
    expect([...DEFAULT_SELECTED_ERROR_STATUSES].sort()).toEqual(
      [...SQL_UNRESOLVED_STATUSES].sort(),
    );
    expect([...UNRESOLVED_ERROR_STATUSES].sort()).toEqual([...SQL_UNRESOLVED_STATUSES].sort());

    // Stated the way the operator sees it: a recurring error must be visible
    // without touching a single pill.
    expect(DEFAULT_SELECTED_ERROR_STATUSES).toContain("recurring");

    // And the statuses the summary does NOT count as unresolved must stay out
    // of the default, or the footer would out-count the header instead.
    for (const closed of ["resolved", "ignored"] as ErrorStatus[]) {
      expect(DEFAULT_SELECTED_ERROR_STATUSES).not.toContain(closed);
    }
  });

  /** And after toggling — the badge must track the pills, not a stale default. */
  it("badge count equals lit pill count after toggling statuses and severities", () => {
    const cases: Array<{ severities: ErrorSeverity[]; statuses: ErrorStatus[] }> = [
      { severities: [], statuses: [] },
      { severities: [], statuses: ["promoted"] },
      { severities: ["critical"], statuses: ["new", "promoted"] },
      { severities: ["critical", "warning"], statuses: [...ALL_STATUSES] },
      { severities: [...ALL_SEVERITIES], statuses: [...ALL_STATUSES] },
    ];

    for (const { severities, statuses } of cases) {
      const lit =
        ERROR_SEVERITY_FILTER_OPTIONS.filter((s) => severities.includes(s)).length +
        ERROR_STATUS_FILTER_OPTIONS.filter((s) => statuses.includes(s)).length;
      expect(filterBadgeCount(severities, statuses)).toBe(lit);
      // Nothing selected may go unrepresented: the badge must never exceed the
      // pills, and must never under-report a selection the user can see lit.
      expect(filterBadgeCount(severities, statuses)).toBe(severities.length + statuses.length);
    }
  });

  /**
   * Item B. The footer's "(filtered from N)" is the only thing on the page that
   * can tell an operator rows exist outside their current filter. Computed from
   * the already-server-filtered list, it reported every server-side exclusion as
   * zero: 6 rows in the store, `recurring` unlit, footer "2 errors (filtered
   * from 2)".
   */
  it("discloses the true corpus size, not the post-filter count", () => {
    // The measured case: 6 stored, 2 survived the server-side status filter.
    expect(disclosedCorpusTotal(6, 2)).toBe(6);
    expect(disclosedCorpusTotal(6, 2)).not.toBe(2);

    // Unfiltered — the two agree, and the footer says nothing surprising.
    expect(disclosedCorpusTotal(6, 6)).toBe(6);

    // Zero is a real total, not a missing one: `?? ` must not swallow it into
    // the fallback, or an empty store would report the loaded count instead.
    expect(disclosedCorpusTotal(0, 4)).toBe(0);

    // Summary not loaded yet -> the caller's own count, the only number we can
    // stand behind at that instant.
    expect(disclosedCorpusTotal(null, 2)).toBe(2);
    expect(disclosedCorpusTotal(undefined, 2)).toBe(2);
  });

  it("every default-selected status has a pill to deselect it with", () => {
    for (const status of DEFAULT_SELECTED_ERROR_STATUSES) {
      expect(ERROR_STATUS_FILTER_OPTIONS).toContain(status);
    }
  });
});

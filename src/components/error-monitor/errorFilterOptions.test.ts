import { describe, it, expect } from "vitest";
import type { ErrorSeverity, ErrorStatus } from "../../types/errorMonitor";
import {
  DEFAULT_SELECTED_ERROR_STATUSES,
  ERROR_SEVERITY_FILTER_OPTIONS,
  ERROR_STATUS_FILTER_OPTIONS,
  filterBadgeCount,
} from "./errorFilterOptions";

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
    expect(filterBadgeCount([], [...DEFAULT_SELECTED_ERROR_STATUSES])).toBe(4);
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

  it("every default-selected status has a pill to deselect it with", () => {
    for (const status of DEFAULT_SELECTED_ERROR_STATUSES) {
      expect(ERROR_STATUS_FILTER_OPTIONS).toContain(status);
    }
  });
});

/**
 * `GlobalIssuesPanel` header stats — plan 2026-08-23-single-source-derived-facts
 * item 6. The header must describe the list below it and name its population.
 */

import { describe, expect, it } from "vitest";

import { computeIssueHeaderStats, type IssueHeaderFilters } from "./issueHeaderStats";

const DEFAULT_FILTERS: IssueHeaderFilters = {
  status: "active",
  category: "all",
  severity: "all",
  searchQuery: "",
};

const ACTIVE_SET = [
  { severity: "critical", status: "active" },
  { severity: "high", status: "active" },
  { severity: "low", status: "active" },
];

describe("computeIssueHeaderStats", () => {
  it("labels the untouched mount view by its status population, not as a total", () => {
    // The server already filtered to status=active at mount; a bare "3" read as
    // a total while silently excluding resolved issues.
    const stats = computeIssueHeaderStats(ACTIVE_SET, ACTIVE_SET, DEFAULT_FILTERS);
    expect(stats.badgeText).toBe("3 active");
    expect(stats.badgeText).not.toBe("3");
    expect(stats.searchActive).toBe(false);
  });

  it("calls the 'all' population a total", () => {
    const all = [...ACTIVE_SET, { severity: "high", status: "resolved" }];
    const stats = computeIssueHeaderStats(all, all, { ...DEFAULT_FILTERS, status: "all" });
    expect(stats.badgeText).toBe("4 total");
    // The resolved high-severity issue is not an active one.
    expect(stats.high).toBe(1);
  });

  it("follows the search: a query matching one issue shows 1, with the pre-search count disclosed", () => {
    const shown = [ACTIVE_SET[1]];
    const stats = computeIssueHeaderStats(ACTIVE_SET, shown, {
      ...DEFAULT_FILTERS,
      searchQuery: "flaky",
    });
    expect(stats.shownCount).toBe(1);
    expect(stats.badgeText).toBe("1 of 3 active");
    // The severity badges describe the list below too — the critical issue was
    // searched away, so it must not be advertised in the header.
    expect(stats.critical).toBe(0);
    expect(stats.high).toBe(1);
  });

  it("treats a whitespace-only query as no search (matches filteredIssues' own short-circuit)", () => {
    const stats = computeIssueHeaderStats(ACTIVE_SET, ACTIVE_SET, {
      ...DEFAULT_FILTERS,
      searchQuery: "   ",
    });
    expect(stats.searchActive).toBe(false);
    expect(stats.badgeText).toBe("3 active");
  });

  it("names every server-side filter in the hover text", () => {
    const stats = computeIssueHeaderStats(ACTIVE_SET, ACTIVE_SET, {
      ...DEFAULT_FILTERS,
      category: "timing",
      severity: "high",
    });
    expect(stats.badgeTitle).toContain("status = active");
    expect(stats.badgeTitle).toContain("category = timing");
    expect(stats.badgeTitle).toContain("severity = high");
  });

  it("counts only ACTIVE critical/high issues", () => {
    const mixed = [
      { severity: "critical", status: "resolved" },
      { severity: "critical", status: "monitoring" },
      { severity: "critical", status: "active" },
    ];
    const stats = computeIssueHeaderStats(mixed, mixed, { ...DEFAULT_FILTERS, status: "all" });
    expect(stats.critical).toBe(1);
  });
});

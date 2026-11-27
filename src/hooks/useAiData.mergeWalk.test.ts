/**
 * Regression test for `mergeWalk` (qontinui-runner#2233).
 *
 * Only the FIRST page of a keyset walk runs the exact count; every later page
 * carries `total: null`. `remaining` must therefore be derived from the first
 * page's total minus every row loaded so far — reading the last page's total
 * blanked the "Show more (N remaining)" label after one click.
 */
import { describe, expect, it } from "vitest";
import { mergeWalk } from "./useAiData";
import type { BoundedReadMeta } from "../types/aiData";

interface Page extends BoundedReadMeta {
  rows: number[];
}

function page(rows: number[], total: number | null, next: string | null): Page {
  return {
    rows,
    count: rows.length,
    limit: 2,
    shown: rows.length,
    total,
    truncated: next !== null,
    bound_kind: total === null ? "at_least" : "exact",
    next_cursor: next,
    available: true,
    filter_narrowed: null,
    enumerate_via: null,
  } as Page;
}

const rowsOf = (p: Page) => p.rows;

describe("mergeWalk", () => {
  it("returns null with no pages", () => {
    expect(mergeWalk<Page, number>(undefined, rowsOf)).toBeNull();
    expect(mergeWalk<Page, number>([], rowsOf)).toBeNull();
  });

  it("derives remaining from the first page's exact total on one page", () => {
    const walk = mergeWalk([page([1, 2], 5, "c1")], rowsOf);
    expect(walk?.remaining).toBe(3);
    expect(walk?.hasMore).toBe(true);
  });

  it("keeps a numeric remaining after later pages report total: null", () => {
    const walk = mergeWalk([page([1, 2], 5, "c1"), page([3, 4], null, "c2")], rowsOf);
    expect(walk?.rows).toEqual([1, 2, 3, 4]);
    expect(walk?.remaining).toBe(1);
    expect(walk?.first.total).toBe(5);
    expect(walk?.hasMore).toBe(true);
  });

  it("reaches zero, never negative, when the walk is exhausted", () => {
    const walk = mergeWalk([page([1, 2], 3, "c1"), page([3, 4], null, null)], rowsOf);
    expect(walk?.remaining).toBe(0);
    expect(walk?.hasMore).toBe(false);
  });

  it("is null when the first page ran no exact count", () => {
    const walk = mergeWalk([page([1, 2], null, "c1"), page([3], null, null)], rowsOf);
    expect(walk?.remaining).toBeNull();
  });
});

/**
 * Page catalog derivation.
 *
 * `getKnownPages()` flattens the shared navigation registry, and the registry
 * legitimately lists some ids TWICE — `run-findings` is both a top-level REVIEW
 * item and a child of Runs, pointing at the same `/runs/findings` page.
 * `getAllItems()` concatenates groups and CHILDREN_MAP without deduping, so the
 * catalog has to.
 */
import { describe, expect, it } from "vitest";

import { getKnownPages } from "../page-catalog";

describe("getKnownPages", () => {
  it("lists each nav id at most once per app", () => {
    const seen = new Map<string, number>();
    for (const page of getKnownPages()) {
      const key = `${page.appName}::${page.navId}`;
      seen.set(key, (seen.get(key) ?? 0) + 1);
    }
    const duplicated = [...seen.entries()].filter(([, count]) => count > 1);
    expect(duplicated).toEqual([]);
  });

  it("still includes an id the registry lists twice", () => {
    // Deduping must keep the page, not drop it.
    const runner = getKnownPages().filter((p) => p.appName === "Qontinui Runner");
    expect(runner.map((p) => p.navId)).toContain("run-findings");
  });

  it("excludes parent containers, which are not pages", () => {
    const ids = getKnownPages().map((p) => p.navId);
    expect(ids).not.toContain("settings");
    expect(ids).not.toContain("runs");
  });
});

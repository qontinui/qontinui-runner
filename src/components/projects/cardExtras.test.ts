import { describe, expect, it } from "vitest";

import { formatWeeklySpend, sortProjects } from "./cardExtras";
import type { SavedProject } from "@/hooks/useSavedProjects";

function proj(overrides: Partial<SavedProject> & { id: string; name: string }): SavedProject {
  return {
    path: `C:/${overrides.id}`,
    projectType: "node",
    manifest: "package.json",
    ...overrides,
  };
}

describe("sortProjects", () => {
  it("puts pinned projects first regardless of recency", () => {
    const out = sortProjects([
      proj({ id: "a", name: "Alpha", lastOpenedMs: 1000 }),
      proj({ id: "b", name: "Beta", pinned: true, lastOpenedMs: 1 }),
    ]);
    expect(out.map((p) => p.id)).toEqual(["b", "a"]);
  });

  it("orders by recency within the same pin state", () => {
    const out = sortProjects([
      proj({ id: "old", name: "Old", lastOpenedMs: 10 }),
      proj({ id: "new", name: "New", lastOpenedMs: 900 }),
      proj({ id: "mid", name: "Mid", lastOpenedMs: 500 }),
    ]);
    expect(out.map((p) => p.id)).toEqual(["new", "mid", "old"]);
  });

  it("sorts a never-opened project after every opened one", () => {
    // `undefined` is "no recency", not "recency zero".
    const out = sortProjects([
      proj({ id: "never", name: "Never" }),
      proj({ id: "opened", name: "Opened", lastOpenedMs: 0 }),
    ]);
    expect(out.map((p) => p.id)).toEqual(["opened", "never"]);
  });

  it("falls back to name when recency ties", () => {
    const out = sortProjects([
      proj({ id: "z", name: "Zebra" }),
      proj({ id: "a", name: "Apple" }),
    ]);
    expect(out.map((p) => p.id)).toEqual(["a", "z"]);
  });

  it("does not mutate the caller's array", () => {
    const input = [proj({ id: "a", name: "A" }), proj({ id: "b", name: "B", pinned: true })];
    const snapshot = input.map((p) => p.id);
    sortProjects(input);
    expect(input.map((p) => p.id)).toEqual(snapshot);
  });
});

describe("formatWeeklySpend", () => {
  it("renders measured spend, including zero", () => {
    // Measured-and-free is a real answer to "is this costing me money?".
    expect(formatWeeklySpend(0)).toBe("$0.00 this week");
    expect(formatWeeklySpend(1.2)).toBe("$1.20 this week");
    expect(formatWeeklySpend(12.345)).toBe("$12.35 this week");
  });

  it("renders nothing when spend was not measured", () => {
    expect(formatWeeklySpend(undefined)).toBeNull();
  });

  it("refuses nonsense rather than rendering it", () => {
    expect(formatWeeklySpend(Number.NaN)).toBeNull();
    expect(formatWeeklySpend(-1)).toBeNull();
    expect(formatWeeklySpend(Number.POSITIVE_INFINITY)).toBeNull();
  });
});

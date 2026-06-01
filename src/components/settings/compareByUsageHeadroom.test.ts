/**
 * Unit tests for `compareByUsageHeadroom` — the "best account" selection
 * comparator. The picker must choose the account with the most headroom
 * relative to its PROJECTED usage (most-negative `usage_delta`), not the
 * one with the lowest raw utilization.
 */
import { describe, expect, it } from "vitest";
import { compareByUsageHeadroom } from "./types";

type A = { utilization: number; usage_delta?: number | null; label?: string };

const best = (accounts: A[]): string =>
  [...accounts].sort(compareByUsageHeadroom)[0]?.label ?? "";

describe("compareByUsageHeadroom", () => {
  it("prefers the account furthest UNDER its projection, not lowest utilization", () => {
    // 'busy' has lower raw utilization (0.40) but is OVER projection (+0.10);
    // 'idle' is higher utilization (0.50) but well UNDER projection (-0.30).
    const accounts: A[] = [
      { label: "busy", utilization: 0.4, usage_delta: 0.1 },
      { label: "idle", utilization: 0.5, usage_delta: -0.3 },
    ];
    expect(best(accounts)).toBe("idle");
  });

  it("orders by usage_delta ascending (most under budget first)", () => {
    const accounts: A[] = [
      { label: "a", utilization: 0.6, usage_delta: -0.1 },
      { label: "b", utilization: 0.6, usage_delta: -0.5 },
      { label: "c", utilization: 0.6, usage_delta: 0.2 },
    ];
    const ordered = [...accounts].sort(compareByUsageHeadroom).map((x) => x.label);
    expect(ordered).toEqual(["b", "a", "c"]);
  });

  it("falls back to raw utilization when usage_delta is null/absent", () => {
    const accounts: A[] = [
      { label: "high", utilization: 0.9, usage_delta: null },
      { label: "low", utilization: 0.2 },
    ];
    expect(best(accounts)).toBe("low");
  });
});

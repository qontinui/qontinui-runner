/**
 * Unit tests for `compareByUsageHeadroom` — the "best account" selection
 * comparator. The picker must choose the account with the most headroom
 * relative to its PROJECTED usage (most-negative `usage_delta`), not the
 * one with the lowest raw utilization.
 */
import { describe, expect, it } from "vitest";
import { compareByUsageHeadroom, isAccountExhausted } from "./types";

type A = {
  utilization: number;
  usage_delta?: number | null;
  status?: string | null;
  error?: string | null;
  label?: string;
};

const best = (accounts: A[]): string => [...accounts].sort(compareByUsageHeadroom)[0]?.label ?? "";

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

  it("a usable account beats an exhausted one with better headroom", () => {
    // 'full' is out of tokens (100% used) but its delta (+0.05) looks better
    // than 'usable' (80% used / 60% expected = +0.20). 'usable' must win — the
    // exhausted account won't serve a request.
    const accounts: A[] = [
      { label: "full", utilization: 1.0, usage_delta: 0.05 },
      { label: "usable", utilization: 0.8, usage_delta: 0.2 },
    ];
    expect(best(accounts)).toBe("usable");
  });

  it("when all exhausted, picks the least-bad headroom among them", () => {
    const accounts: A[] = [
      { label: "a", utilization: 1.0, usage_delta: 0.3 },
      { label: "b", utilization: 1.0, usage_delta: 0.1 },
    ];
    expect(best(accounts)).toBe("b");
  });

  it("treats a rejected status or probe error as exhausted", () => {
    const rejected: A[] = [
      { label: "rejected", utilization: 0.5, status: "rejected", usage_delta: -0.4 },
      { label: "ok", utilization: 0.7, usage_delta: 0.1 },
    ];
    expect(best(rejected)).toBe("ok");

    const errored: A[] = [
      {
        label: "errored",
        utilization: 0.1,
        error: "API error (429): rate limit",
        usage_delta: -0.5,
      },
      { label: "ok", utilization: 0.7, usage_delta: 0.1 },
    ];
    expect(best(errored)).toBe("ok");
  });
});

describe("isAccountExhausted", () => {
  it("is false for a usable, allowed account", () => {
    expect(isAccountExhausted({ utilization: 0.8, status: "allowed" })).toBe(false);
    expect(isAccountExhausted({ utilization: 0.98, status: "allowed_warning" })).toBe(false);
  });

  it("is true at/over the weekly cap", () => {
    expect(isAccountExhausted({ utilization: 0.99 })).toBe(true);
    expect(isAccountExhausted({ utilization: 1.0 })).toBe(true);
  });

  it("is true on probe error or rejected/blocked status", () => {
    expect(isAccountExhausted({ utilization: 0.1, error: "boom" })).toBe(true);
    expect(isAccountExhausted({ utilization: 0.5, status: "rejected" })).toBe(true);
    expect(isAccountExhausted({ utilization: 0.5, status: "BLOCKED" })).toBe(true);
  });
});

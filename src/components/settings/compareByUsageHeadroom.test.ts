/**
 * Unit tests for `compareByUsageHeadroom` — the "best account" selection
 * comparator, and the mirror of the runner's `cmp_rank`
 * (`src-tauri/src/ai_provider/config.rs`). These cases are deliberately the
 * same cases as the Rust suite, so a drift between the two mirrors shows up as
 * one side going red.
 *
 * The rule under test is **use-it-or-lose-it**: unused weekly capacity expires
 * at the account's reset and does not roll over, so among accounts under their
 * projected pace the picker must choose the one whose 7-day window is furthest
 * along (**highest** `expected_utilization`) — the account whose spare capacity
 * is about to be lost — not the emptiest one and not the one furthest under its
 * projection (most-negative `usage_delta`), which is the displaced rule.
 *
 * The over-pace fallback ranks by the **ratio** `utilization /
 * expected_utilization` ascending, not by the difference: a difference is not
 * comparable across accounts at different points in their windows. The
 * constructed X/Y pair below is the case that tells the two apart.
 */
import { describe, expect, it } from "vitest";
import { compareByUsageHeadroom, isAccountExhausted } from "./types";

type A = {
  utilization: number;
  expected_utilization?: number | null;
  usage_delta?: number | null;
  status?: string | null;
  error?: string | null;
  label?: string;
};

const order = (accounts: A[]): string[] =>
  [...accounts].sort(compareByUsageHeadroom).map((x) => x.label ?? "");

const best = (accounts: A[]): string => order(accounts)[0] ?? "";

describe("compareByUsageHeadroom", () => {
  // ── Level 3a — within under-pace, highest `expected_utilization` wins ─────
  //
  // Unused weekly capacity expires at the account's reset and does not roll
  // over, so the account to burn is the one whose window is furthest along.
  // These two cases replace the displaced min-`usage_delta` ones.

  it("prefers the HIGHEST-expected under-pace account, not the emptiest", () => {
    // 'fresh' has far more absolute room (10% used) and a much better delta
    // (-0.30 vs -0.02), but its window has barely started — its spare
    // capacity is in no danger of expiring. 'expiring' is nearly through its
    // week with spare capacity about to be lost outright, so it wins.
    const accounts: A[] = [
      { label: "fresh", utilization: 0.1, expected_utilization: 0.4, usage_delta: -0.3 },
      { label: "expiring", utilization: 0.79, expected_utilization: 0.8, usage_delta: -0.01 },
    ];
    expect(best(accounts)).toBe("expiring");
  });

  it("orders under-pace accounts by expected_utilization DESCENDING", () => {
    const accounts: A[] = [
      { label: "a", utilization: 0.3, expected_utilization: 0.4, usage_delta: -0.1 },
      { label: "b", utilization: 0.7, expected_utilization: 0.9, usage_delta: -0.2 },
      { label: "c", utilization: 0.05, expected_utilization: 0.1, usage_delta: -0.05 },
    ];
    expect(order(accounts)).toEqual(["b", "a", "c"]);
  });

  it("every under-pace account sorts ahead of every over-pace account", () => {
    // Level 2 dominates level 3: 'under' has the LOWEST expected in the
    // roster, and still beats an over-pace account with a far higher one.
    const accounts: A[] = [
      { label: "over", utilization: 0.85, expected_utilization: 0.8, usage_delta: 0.05 },
      { label: "under", utilization: 0.02, expected_utilization: 0.05, usage_delta: -0.03 },
    ];
    expect(best(accounts)).toBe("under");
  });

  // ── Level 3b — the Unknown tier ──────────────────────────────────────────

  it("falls back to raw utilization when usage_delta is null/absent", () => {
    const accounts: A[] = [
      { label: "high", utilization: 0.9, usage_delta: null },
      { label: "low", utilization: 0.2 },
    ];
    expect(best(accounts)).toBe("low");
  });

  it("an account with no expected_utilization lands in its OWN middle tier", () => {
    // Unknown is neither under- nor over-pace: it never beats a measured
    // under-pace account, and never loses to a measured over-pace one. Note
    // 'unknown' here has a NEGATIVE usage_delta — a missing expected value
    // alone is enough to demote it out of the under-pace tier, because 3a's
    // key is uncomputable for it.
    const accounts: A[] = [
      { label: "over", utilization: 0.85, expected_utilization: 0.8, usage_delta: 0.05 },
      { label: "unknown", utilization: 0.5, usage_delta: -0.2 },
      { label: "under", utilization: 0.3, expected_utilization: 0.4, usage_delta: -0.1 },
    ];
    expect(order(accounts)).toEqual(["under", "unknown", "over"]);
  });

  it("orders Unknown-tier accounts by raw utilization ascending", () => {
    const accounts: A[] = [
      { label: "busy", utilization: 0.8 },
      { label: "quiet", utilization: 0.1 },
      { label: "mid", utilization: 0.45, usage_delta: null, expected_utilization: null },
    ];
    expect(order(accounts)).toEqual(["quiet", "mid", "busy"]);
  });

  // ── Level 3c — within over-pace, the RATIO ascending ─────────────────────

  it("ranks over-pace accounts by ratio, not by difference", () => {
    // The constructed pair the measured roster below CANNOT distinguish.
    // X is +0.04 over with a nearly-full week to go (40% past its own pace);
    // Y is +0.05 over with the week almost done (6% past its own pace).
    // The displaced `usage_delta`-ascending key picks X (0.04 < 0.05); the
    // ratio rule picks Y (1.063 < 1.400).
    //
    // ⚠️ An assertion of "X" here means the code reverted to the displaced
    // difference key — the ratio is the whole point of this tier.
    const accounts: A[] = [
      { label: "X", utilization: 0.14, expected_utilization: 0.1, usage_delta: 0.04 },
      { label: "Y", utilization: 0.85, expected_utilization: 0.8, usage_delta: 0.05 },
    ];
    expect(best(accounts)).toBe("Y");
  });

  it("still returns a pick when NO account is under pace", () => {
    // The operator's actual sentence: "if no accounts have less than expected
    // token usage, fall back to the ratio calculation." Ratios: mild 1.050,
    // steep 1.638, moderate 1.082.
    const accounts: A[] = [
      { label: "steep", utilization: 0.79, expected_utilization: 0.4822, usage_delta: 0.3078 },
      { label: "moderate", utilization: 0.76, expected_utilization: 0.7025, usage_delta: 0.0575 },
      { label: "mild", utilization: 0.63, expected_utilization: 0.6, usage_delta: 0.03 },
    ];
    expect(order(accounts)).toEqual(["mild", "moderate", "steep"]);
  });

  // ── The `expected_utilization === 0` guard ───────────────────────────────
  //
  // `expected === 0` forces `delta === utilization >= 0`, so it can only ever
  // arise in the over-pace tier — exactly where the ratio divides.

  it("defines the ratio at expected === 0: (0,0) first, (>0,0) last", () => {
    const accounts: A[] = [
      { label: "spent-at-reset", utilization: 0.5, expected_utilization: 0, usage_delta: 0.5 },
      { label: "mid", utilization: 0.6, expected_utilization: 0.5, usage_delta: 0.1 },
      // Exactly on pace with nothing spent — ratio defined as 1.0, so it is
      // literally the least-over account in the tier.
      { label: "untouched-at-reset", utilization: 0, expected_utilization: 0, usage_delta: 0 },
    ];
    expect(order(accounts)).toEqual(["untouched-at-reset", "mid", "spent-at-reset"]);
  });

  it("never returns NaN — a NaN comparator result is a silent mis-sort", () => {
    // `Array.prototype.sort` treats a NaN return as 0, so an unguarded
    // `0 / 0` would not throw; it would make the order depend on input
    // position instead of on the data. This is the guard's regression test.
    const zeroZero: A = { label: "zz", utilization: 0, expected_utilization: 0, usage_delta: 0 };
    const zeroPos: A = { label: "zp", utilization: 0.5, expected_utilization: 0, usage_delta: 0.5 };
    const normal: A = { label: "n", utilization: 0.6, expected_utilization: 0.5, usage_delta: 0.1 };

    for (const [x, y] of [
      [zeroZero, zeroZero],
      [zeroZero, zeroPos],
      [zeroPos, zeroZero],
      // Infinity vs Infinity: a subtraction-based comparator returns NaN here.
      [zeroPos, zeroPos],
      [zeroZero, normal],
      [zeroPos, normal],
    ] as const) {
      expect(Number.isNaN(compareByUsageHeadroom(x, y))).toBe(false);
    }

    // Two Infinity ratios are equal, not unordered.
    const zeroPos2: A = { ...zeroPos, label: "zp2" };
    expect(compareByUsageHeadroom(zeroPos, zeroPos2)).toBe(0);
  });

  // ── The measured roster ──────────────────────────────────────────────────

  it("orders the measured merytshost roster: paktis first, hotmail fourth", () => {
    // Source: `GET http://127.0.0.1:9876/analytics/account-usage` on
    // merytshost, 2026-09-01 (source `oauth_usage`). Re-measure there if this
    // ever needs updating. The shipped min-delta key picked `.claude-hotmail`
    // — dead last among the under-pace set under the intended rule.
    const roster: A[] = [
      {
        label: ".claude-paktis",
        utilization: 0.79,
        expected_utilization: 0.8037,
        usage_delta: -0.0137,
      },
      {
        label: ".claude-iris",
        utilization: 0.56,
        expected_utilization: 0.6489,
        usage_delta: -0.0889,
      },
      {
        label: ".claude-qontinui",
        utilization: 0.5,
        expected_utilization: 0.5894,
        usage_delta: -0.0894,
      },
      {
        label: ".claude-hotmail",
        utilization: 0.29,
        expected_utilization: 0.381,
        usage_delta: -0.091,
      },
      {
        label: ".claude-pakqon",
        utilization: 0.04,
        expected_utilization: 0.0596,
        usage_delta: -0.0196,
      },
      // Exhausted at 1.00 weekly — the dominating tier excludes it before its
      // ratio (1.208) is ever consulted.
      { label: ".claude", utilization: 1.0, expected_utilization: 0.8275, usage_delta: 0.1725 },
      {
        label: ".claude-paktis-gmail",
        utilization: 0.76,
        expected_utilization: 0.7025,
        usage_delta: 0.0575,
      },
      {
        label: ".claude-tiohorst",
        utilization: 0.79,
        expected_utilization: 0.4822,
        usage_delta: 0.3078,
      },
    ];

    const ordered = order(roster);
    expect(ordered[0]).toBe(".claude-paktis");
    expect(ordered[3]).toBe(".claude-hotmail");
    expect(ordered).toEqual([
      ".claude-paktis",
      ".claude-iris",
      ".claude-qontinui",
      ".claude-hotmail",
      ".claude-pakqon",
      ".claude-paktis-gmail",
      ".claude-tiohorst",
      ".claude",
    ]);
  });

  // ── Level 1 — exhaustion dominates (unchanged) ───────────────────────────

  it("a usable account beats an exhausted one with a better pace key", () => {
    // 'full' is out of tokens (100% used) but its delta (+0.05) looks better
    // than 'usable' (80% used / 60% expected = +0.20). 'usable' must win — the
    // exhausted account won't serve a request.
    const accounts: A[] = [
      { label: "full", utilization: 1.0, expected_utilization: 0.95, usage_delta: 0.05 },
      { label: "usable", utilization: 0.8, expected_utilization: 0.6, usage_delta: 0.2 },
    ];
    expect(best(accounts)).toBe("usable");
  });

  it("when all exhausted, picks the least-bad among them", () => {
    const accounts: A[] = [
      { label: "a", utilization: 1.0, expected_utilization: 0.7, usage_delta: 0.3 },
      { label: "b", utilization: 1.0, expected_utilization: 0.9, usage_delta: 0.1 },
    ];
    expect(best(accounts)).toBe("b");
  });

  it("treats a rejected status or probe error as exhausted", () => {
    const rejected: A[] = [
      {
        label: "rejected",
        utilization: 0.5,
        status: "rejected",
        expected_utilization: 0.9,
        usage_delta: -0.4,
      },
      { label: "ok", utilization: 0.7, expected_utilization: 0.6, usage_delta: 0.1 },
    ];
    expect(best(rejected)).toBe("ok");

    const errored: A[] = [
      {
        label: "errored",
        utilization: 0.1,
        error: "API error (429): rate limit",
        expected_utilization: 0.6,
        usage_delta: -0.5,
      },
      { label: "ok", utilization: 0.7, expected_utilization: 0.6, usage_delta: 0.1 },
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

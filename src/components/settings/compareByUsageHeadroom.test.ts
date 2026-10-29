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
import {
  compareByHighestExpectedUsage,
  compareByUsageHeadroom,
  compareForAccountSelectionMode,
  headroomFloorsFromSettings,
  isAccountExhausted,
  MIN_FLOOR_SETTING,
  MIN_HEADROOM,
  MIN_SHORTFALL,
} from "./types";

type A = {
  utilization: number;
  expected_utilization?: number | null;
  usage_delta?: number | null;
  status?: string | null;
  error?: string | null;
  label?: string;
};

type B = A & { expected_utilization?: number | null };

const order = (accounts: A[]): string[] =>
  [...accounts].sort(compareByUsageHeadroom).map((x) => x.label ?? "");

const best = (accounts: A[]): string => order(accounts)[0] ?? "";

const bestExpected = (accounts: B[]): string =>
  [...accounts].sort(compareByHighestExpectedUsage)[0]?.label ?? "";

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
    // (-0.30 vs -0.05), but its window has barely started. 'expiring' is
    // under pace past MIN_SHORTFALL, so the window furthest along wins.
    const accounts: A[] = [
      { label: "fresh", utilization: 0.1, expected_utilization: 0.4, usage_delta: -0.3 },
      { label: "expiring", utilization: 0.75, expected_utilization: 0.8, usage_delta: -0.05 },
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

  it("orders the measured merytshost roster: iris first, hotmail third", () => {
    // Source: `GET http://127.0.0.1:9876/analytics/account-usage` on
    // merytshost, 2026-09-01 (source `oauth_usage`). Re-measure there if this
    // ever needs updating. The shipped min-delta key picked `.claude-hotmail`
    // — dead last among the under-pace set under the intended rule.
    //
    // Since MIN_SHORTFALL (runner `measured_roster_2026_09_01_picks_iris_not_hotmail`):
    // paktis (-0.0137) and pakqon (-0.0196) are under pace by less than the
    // floor, so they are demoted to the over-pace tier and ranked on their
    // ratios (pakqon 0.671, paktis 0.983).
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
    expect(ordered[0]).toBe(".claude-iris");
    expect(ordered[2]).toBe(".claude-hotmail");
    expect(ordered).toEqual([
      ".claude-iris",
      ".claude-qontinui",
      ".claude-hotmail",
      ".claude-pakqon",
      ".claude-paktis",
      ".claude-paktis-gmail",
      ".claude-tiohorst",
      ".claude",
    ]);
  });

  // ── The headroom floors (runner `under_pace_shortfall`) ──────────────────

  it("the 2026-10-18 incident: pakqon is never under pace, and MIN_SHORTFALL is why", () => {
    // Runner `incident_2026_10_18_pakqon_is_never_under_pace_and_min_shortfall_is_why`.
    // pakqon beside an account with a real margin and an earlier window: under
    // the displaced `delta < 0` rule pakqon (the later window) won the
    // under-pace rows; with the floor the margin account wins every row.
    const rows: [string, number, number][] = [
      ["20:41", 0.81, 0.8136],
      ["21:34", 0.82, 0.8189],
      ["22:03", 0.82, 0.8217],
      ["22:29:56", 0.82, 0.8243],
      ["23:31", 0.84, 0.8305],
    ];
    const margin: A = {
      label: "margin",
      utilization: 0.4,
      expected_utilization: 0.5,
      usage_delta: -0.1,
    };
    const noShortfallFloor = { min_shortfall: Number.MIN_VALUE, min_headroom: MIN_HEADROOM };
    for (const [at, utilization, expected] of rows) {
      const pakqon: A = {
        label: `pakqon@${at}`,
        utilization,
        expected_utilization: expected,
        usage_delta: utilization - expected,
      };
      expect(best([pakqon, margin])).toBe("margin");
      if (utilization - expected < 0) {
        // With MIN_SHORTFALL relaxed (headroom floor kept) it wins again: the
        // shortfall floor, not the headroom floor, is what excluded it.
        const relaxed = [pakqon, margin].sort((a, b) =>
          compareByUsageHeadroom(a, b, noShortfallFloor),
        );
        expect(relaxed[0]?.label).toBe(pakqon.label);
      }
    }
  });

  it("an account under MIN_HEADROOM sorts after every usable account but before an exhausted one", () => {
    // Runner `cmp_rank_low_headroom_sorts_after_every_usable_tier_but_before_exhausted`.
    // 'thin' is 6 points under pace with a late window — the best under-pace
    // key in the roster — but has only 7 points left.
    const thin: A = {
      label: "thin",
      utilization: 0.93,
      expected_utilization: 0.99,
      usage_delta: -0.06,
    };
    const accounts: A[] = [
      thin,
      { label: "over", utilization: 0.5, expected_utilization: 0.1, usage_delta: 0.4 },
      { label: "unknown", utilization: 0.8 },
      { label: "dead", utilization: 1.0, expected_utilization: 0.9, usage_delta: 0.1 },
    ];
    expect(order(accounts)).toEqual(["unknown", "over", "thin", "dead"]);
    // Still the pick when it is the only non-exhausted account.
    expect(best([accounts[3]!, thin])).toBe("thin");
    expect(1 - thin.utilization).toBeLessThan(MIN_HEADROOM);
    // The highest-expected comparator agrees.
    const hexp = [...accounts].sort(compareByHighestExpectedUsage).map((x) => x.label);
    expect(hexp.indexOf("thin")).toBe(2);
    expect(hexp[3]).toBe("dead");
  });

  it("honours the settings overrides, clamped like the runner", () => {
    expect(headroomFloorsFromSettings({})).toEqual({
      min_shortfall: MIN_SHORTFALL,
      min_headroom: MIN_HEADROOM,
    });
    expect(
      headroomFloorsFromSettings({ selection_min_shortfall: 0, selection_min_headroom: 7 }),
    ).toEqual({ min_shortfall: MIN_FLOOR_SETTING, min_headroom: 1 });
    expect(
      headroomFloorsFromSettings({ selection_min_shortfall: null, selection_min_headroom: NaN }),
    ).toEqual({ min_shortfall: MIN_SHORTFALL, min_headroom: MIN_HEADROOM });

    // A 3-point margin: under pace by default, demoted under a 5-point floor.
    const close: A = {
      label: "close",
      utilization: 0.77,
      expected_utilization: 0.8,
      usage_delta: -0.03,
    };
    const early: A = {
      label: "early",
      utilization: 0.1,
      expected_utilization: 0.3,
      usage_delta: -0.2,
    };
    expect(best([close, early])).toBe("close");
    const strict = headroomFloorsFromSettings({ selection_min_shortfall: 0.05 });
    const cmp = compareForAccountSelectionMode("least_usage", strict);
    expect([close, early].sort(cmp)[0]?.label).toBe("early");
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

describe("compareByHighestExpectedUsage", () => {
  it("picks the highest expected_utilization among under-pace accounts, not the lowest utilization", () => {
    const accounts: B[] = [
      { label: "low-expected", utilization: 0.2, usage_delta: -0.1, expected_utilization: 0.3 },
      { label: "high-expected", utilization: 0.5, usage_delta: -0.4, expected_utilization: 0.9 },
    ];
    expect(bestExpected(accounts)).toBe("high-expected");
  });

  it("never picks an account at or over its expected pace when an under-pace one exists", () => {
    // 'over' has the highest expected_utilization but is OVER pace (positive
    // delta) → must lose to 'under', which qualifies for the primary tier.
    const accounts: B[] = [
      { label: "over", utilization: 0.95, usage_delta: 0.1, expected_utilization: 0.85 },
      { label: "under", utilization: 0.2, usage_delta: -0.1, expected_utilization: 0.3 },
    ];
    expect(bestExpected(accounts)).toBe("under");
  });

  it("treats delta exactly zero (at pace) as not under pace", () => {
    const accounts: B[] = [
      { label: "at-pace", utilization: 0.5, usage_delta: 0, expected_utilization: 0.5 },
      { label: "under-pace", utilization: 0.1, usage_delta: -0.05, expected_utilization: 0.15 },
    ];
    expect(bestExpected(accounts)).toBe("under-pace");
  });

  it("a usable account beats an exhausted one regardless of expected usage", () => {
    const accounts: B[] = [
      { label: "full", utilization: 1.0, usage_delta: -0.05, expected_utilization: 1.05 },
      { label: "usable", utilization: 0.2, usage_delta: -0.1, expected_utilization: 0.3 },
    ];
    expect(bestExpected(accounts)).toBe("usable");
  });

  it("falls back to headroom order when no account qualifies as under-pace", () => {
    const accounts: B[] = [
      { label: "a", utilization: 0.6, usage_delta: 0.2, expected_utilization: 0.4 },
      { label: "b", utilization: 0.6, usage_delta: 0.05, expected_utilization: 0.55 },
    ];
    // Neither is under pace, so this falls back to compareByUsageHeadroom's
    // ascending-delta order: 'b' (smaller positive delta) sorts first.
    expect(bestExpected(accounts)).toBe("b");
  });

  it("falls back to headroom order when expected_utilization is unknown for both", () => {
    const accounts: B[] = [
      { label: "a", utilization: 0.6, usage_delta: -0.2 }, // no expected_utilization
      { label: "b", utilization: 0.3, usage_delta: -0.1 }, // no expected_utilization
    ];
    // Neither can qualify for the primary tier without expected_utilization →
    // falls back to headroom order, where 'a' (-0.2) beats 'b' (-0.1).
    expect(bestExpected(accounts)).toBe("a");
  });

  it("an account missing expected_utilization loses to one that has it, even with worse headroom", () => {
    const accounts: B[] = [
      { label: "no-expected", utilization: 0.6, usage_delta: -0.2 }, // no expected_utilization → tier 1
      { label: "has-expected", utilization: 0.3, usage_delta: -0.1, expected_utilization: 0.4 }, // tier 0
    ];
    expect(bestExpected(accounts)).toBe("has-expected");
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

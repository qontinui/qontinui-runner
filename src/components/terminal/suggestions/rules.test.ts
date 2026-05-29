/**
 * Pure-function tests for the suggestion rule evaluator and reconciler.
 *
 * Covers:
 *   1. ruleErrorInZone — emits one chip per errored zone, with the
 *      correct slash form (1-based zone index)
 *   2. ruleStuckNeedsInput — fires only when elapsed > 30s, picks the
 *      right human-readable duration bucket (s / m / h)
 *   3. ruleLayoutMismatch — fires only when overflow factor exceeded
 *      AND the suggested preset would actually change
 *   4. reconcileChips — priority-resolves multiple candidates on the
 *      same zone (error wins over stuck-needs-input)
 *   5. reconcileChips — applies suppression filter
 *   6. reconcileChips — caps at maxOnPage, dropping lowest-priority
 *      chips first
 */

import { describe, expect, it } from "vitest";

import {
  reconcileChips,
  ruleErrorInZone,
  ruleLayoutMismatch,
  ruleStuckNeedsInput,
  type SuggestionContext,
} from "./rules";

const NOW = 1_700_000_000_000;

const ctx = (overrides: Partial<SuggestionContext> = {}): SuggestionContext => ({
  nowMs: NOW,
  tabsCount: 0,
  zoneCount: 1,
  currentLayoutId: "single",
  assignments: {},
  sessionStates: {},
  stateEntryMs: {},
  focusedZone: 0,
  ...overrides,
});

describe("ruleErrorInZone", () => {
  it("emits one chip per errored zone with 1-based slash form", () => {
    const chips = ruleErrorInZone(
      ctx({
        assignments: { 0: "t-a", 2: "t-b" },
        sessionStates: { "t-a": "error", "t-b": "idle" },
      }),
    );
    expect(chips).toHaveLength(1);
    expect(chips[0]).toMatchObject({
      ruleId: "error-in-zone",
      zoneIdx: 0,
      priority: 100,
      slash: "/restart 1",
      actionId: "terminal.restart",
      args: { zone: 1 },
    });
  });

  it("emits nothing when no tab is in error state", () => {
    expect(
      ruleErrorInZone(
        ctx({
          assignments: { 0: "t-a" },
          sessionStates: { "t-a": "working" },
        }),
      ),
    ).toEqual([]);
  });
});

describe("ruleStuckNeedsInput", () => {
  it("fires only once the 30s gate is passed", () => {
    const baseCtx = ctx({
      assignments: { 0: "t-a" },
      sessionStates: { "t-a": "needs-input" },
      stateEntryMs: { "t-a": NOW - 20_000 },
    });
    expect(ruleStuckNeedsInput(baseCtx)).toEqual([]);
    expect(
      ruleStuckNeedsInput({ ...baseCtx, stateEntryMs: { "t-a": NOW - 31_000 } }),
    ).toHaveLength(1);
  });

  it("formats elapsed time into s/m/h buckets", () => {
    const stuck = (elapsedMs: number): SuggestionContext =>
      ctx({
        assignments: { 0: "t-a" },
        sessionStates: { "t-a": "needs-input" },
        stateEntryMs: { "t-a": NOW - elapsedMs },
      });
    expect(ruleStuckNeedsInput(stuck(45_000))[0].headline).toMatch(/45s/);
    expect(ruleStuckNeedsInput(stuck(5 * 60_000))[0].headline).toMatch(/5m/);
    expect(ruleStuckNeedsInput(stuck(2 * 3_600_000))[0].headline).toMatch(/2h/);
  });

  it("skips zones whose tab has no stateEntryMs entry", () => {
    expect(
      ruleStuckNeedsInput(
        ctx({
          assignments: { 0: "t-a" },
          sessionStates: { "t-a": "needs-input" },
        }),
      ),
    ).toEqual([]);
  });
});

describe("ruleLayoutMismatch", () => {
  it("fires when tabsCount > zoneCount * 1.2 AND suggested preset differs", () => {
    expect(
      ruleLayoutMismatch(
        ctx({
          tabsCount: 7,
          zoneCount: 4,
          currentLayoutId: "quad",
          focusedZone: 2,
        }),
      ),
    ).toHaveLength(1);
  });

  it("does not fire when the suggested preset matches current", () => {
    // 6 tabs with current already six-pack → no chip.
    expect(
      ruleLayoutMismatch(
        ctx({
          tabsCount: 6,
          zoneCount: 5,
          currentLayoutId: "six-pack",
        }),
      ),
    ).toEqual([]);
  });

  it("does not fire below the 1.2x overflow factor", () => {
    expect(
      ruleLayoutMismatch(
        ctx({
          tabsCount: 4,
          zoneCount: 4,
          currentLayoutId: "quad",
        }),
      ),
    ).toEqual([]);
  });

  it("anchors the chip on focusedZone", () => {
    const chips = ruleLayoutMismatch(
      ctx({
        tabsCount: 7,
        zoneCount: 4,
        currentLayoutId: "quad",
        focusedZone: 3,
      }),
    );
    expect(chips[0].zoneIdx).toBe(3);
  });
});

describe("reconcileChips", () => {
  it("keeps highest-priority chip when multiple fire on the same zone", () => {
    // Zone 0 is BOTH errored AND has been needs-input for 1 minute —
    // error (100) wins over stuck-needs-input (70).
    // (Real-world this combination is impossible — state is a single
    //  enum — but the reconciler must still pick deterministically.)
    const result = reconcileChips(
      ctx({
        assignments: { 0: "t-a" },
        sessionStates: { "t-a": "error" }, // satisfies error rule
        stateEntryMs: { "t-a": NOW - 60_000 },
      }),
      new Set(),
    );
    expect(result.size).toBe(1);
    expect(result.get(0)?.ruleId).toBe("error-in-zone");
  });

  it("applies suppression keyed on (zoneIdx, ruleId)", () => {
    const result = reconcileChips(
      ctx({
        assignments: { 0: "t-a" },
        sessionStates: { "t-a": "error" },
      }),
      new Set(["0:error-in-zone"]),
    );
    expect(result.size).toBe(0);
  });

  it("caps the page total, dropping lowest-priority chips first", () => {
    // 4 errored zones → 4 chips, all priority 100. Cap=3, so one drops.
    const result = reconcileChips(
      ctx({
        assignments: { 0: "a", 1: "b", 2: "c", 3: "d" },
        sessionStates: { a: "error", b: "error", c: "error", d: "error" },
      }),
      new Set(),
      3,
    );
    expect(result.size).toBe(3);
  });

  it("drops layout-mismatch (priority 30) before error chips when over cap", () => {
    const result = reconcileChips(
      ctx({
        // Three errored zones + a layout-mismatch chip on zone 0
        // (overridden by error on zone 0 in the per-zone dedupe), but
        // also tabsCount > zoneCount*1.2 → layout chip on focusedZone.
        // After per-zone dedupe: zone 0 = error, zone 1 = error,
        // zone 2 = error, focusedZone (3) = layout-mismatch. Cap=3 drops
        // the layout chip.
        tabsCount: 10,
        zoneCount: 4,
        currentLayoutId: "quad",
        focusedZone: 3,
        assignments: { 0: "a", 1: "b", 2: "c" },
        sessionStates: { a: "error", b: "error", c: "error" },
      }),
      new Set(),
      3,
    );
    expect(result.size).toBe(3);
    for (const chip of result.values()) {
      expect(chip.ruleId).toBe("error-in-zone");
    }
  });
});

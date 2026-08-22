/**
 * Pure-helper tests for the StatusStrip's two count reconciliations.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so the strip
 * itself can't be rendered here — the counting rules are extracted as pure
 * functions precisely so the load-bearing logic is testable without a DOM.
 */

import { describe, expect, it } from "vitest";

import { countTabsInState, splitNeedsInput, unionErrorCount } from "./StatusStrip";

describe("countTabsInState", () => {
  it("counts only tabs that are still in the live tab list", () => {
    // A stale `sessionStates` entry for a closed tab must not inflate a pill.
    const states = { a: "error", b: "idle", ghost: "error" } as const;
    expect(countTabsInState([{ id: "a" }, { id: "b" }], states, "error")).toBe(1);
  });

  it("is 0 for missing inputs rather than throwing", () => {
    expect(countTabsInState(undefined, undefined, "error")).toBe(0);
    expect(countTabsInState([{ id: "a" }], null, "error")).toBe(0);
  });
});

describe("unionErrorCount", () => {
  it("surfaces a dead PTY tab the session bucketing never saw — THE DEFECT", () => {
    // statusCounts buckets Claude sessions; a tab whose PTY died has no live
    // session, so the pill read 0 while the page painted the tab red and
    // focusNextError would cycle to it.
    expect(unionErrorCount(0, 1)).toBe(1);
  });

  it("does not double-count an error both surfaces can see", () => {
    // The sets overlap and share no key to dedupe on, so max — not sum — is
    // the honest union.
    expect(unionErrorCount(1, 1)).toBe(1);
  });

  it("keeps a session-only error visible", () => {
    expect(unionErrorCount(2, 0)).toBe(2);
  });
});

describe("splitNeedsInput", () => {
  it("headlines the count the cycler and BatchActions can actually reach", () => {
    // 2 waiting sessions, only 1 of them backed by a tab in this window:
    // "2 need input · Tab to cycle" was a claim the controls could not honour.
    expect(splitNeedsInput(2, 1)).toEqual({ actionable: 1, external: 1 });
  });

  it("reports a purely external waiter instead of dropping it", () => {
    expect(splitNeedsInput(2, 0)).toEqual({ actionable: 0, external: 2 });
  });

  it("never reports a negative surplus when tabs lead the session model", () => {
    // The two models settle at different times; a tab-scoped count ahead of
    // the session bucketing must not render "+-1 external".
    expect(splitNeedsInput(0, 2)).toEqual({ actionable: 2, external: 0 });
  });
});

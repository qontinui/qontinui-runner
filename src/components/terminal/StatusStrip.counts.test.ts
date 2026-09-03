/**
 * Pure-helper tests for the StatusStrip's two count reconciliations.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so the strip
 * itself can't be rendered here — the counting rules are extracted as pure
 * functions precisely so the load-bearing logic is testable without a DOM.
 */

import { describe, expect, it } from "vitest";

import {
  countLiveTabs,
  countTabsInState,
  splitNeedsInput,
  unionErrorCount,
  unionSessionCount,
} from "./StatusStrip";

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

describe("countLiveTabs", () => {
  it("counts the tabs whose PTY is still running", () => {
    expect(countLiveTabs([{ isAlive: true }, { isAlive: true }])).toBe(2);
  });

  it("does not count an exited PTY's tombstone tab", () => {
    // A dead tab is not a place the operator can work, so it must not reopen
    // the strip on a page with nothing running.
    expect(countLiveTabs([{ isAlive: true }, { isAlive: false }])).toBe(1);
  });

  it("treats an absent `isAlive` as not-live rather than assuming liveness", () => {
    // Same reading as `buildTerminalSessionRoster`'s `Boolean(t.isAlive)`.
    expect(countLiveTabs([{}, { isAlive: undefined }])).toBe(0);
  });

  it("is 0 for missing inputs rather than throwing", () => {
    expect(countLiveTabs(undefined)).toBe(0);
    expect(countLiveTabs(null)).toBe(0);
    expect(countLiveTabs([])).toBe(0);
  });
});

describe("unionSessionCount", () => {
  it("surfaces live PTY tabs the Claude-session bucketing never saw — THE DEFECT", () => {
    // Two terminals open, no Claude session attached to either: `sessionCount`
    // is 0, so `isMultiZone` was false and `hasContent` hid the whole strip on
    // a page that visibly had two sessions in it.
    expect(unionSessionCount(0, 2)).toBe(2);
    expect(unionSessionCount(0, 2) > 1).toBe(true);
  });

  it("does not double-count a session both surfaces can see", () => {
    // The sets overlap and share no key to dedupe on, so max — not sum.
    expect(unionSessionCount(2, 2)).toBe(2);
  });

  it("keeps an external Claude session with no tab in this window visible", () => {
    expect(unionSessionCount(3, 1)).toBe(3);
  });

  it("stays single-zone when there is genuinely one session", () => {
    expect(unionSessionCount(1, 1) > 1).toBe(false);
    expect(unionSessionCount(0, 1) > 1).toBe(false);
    expect(unionSessionCount(0, 0) > 1).toBe(false);
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

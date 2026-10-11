/**
 * The pure tmux work-mode rules (plan `2026-10-11-runner-terminal-tmux-work-mode`,
 * Phase 1). Every case asserts the literal resulting state.
 */

import { describe, expect, it } from "vitest";
import {
  MRU_LIMIT,
  nextShownAfterClose,
  parseGridSnapshot,
  parseMru,
  parseWorkMode,
  pushMru,
  reconcileTmuxAssignments,
  restoreGridSnapshot,
  shownTabOnEnter,
} from "./terminalWorkMode";

describe("parseWorkMode", () => {
  it("absent (undefined or null) is grid, not malformed", () => {
    expect(parseWorkMode(undefined)).toStrictEqual({ mode: "grid", malformed: false });
    expect(parseWorkMode(null)).toStrictEqual({ mode: "grid", malformed: false });
  });

  it("known values parse as themselves", () => {
    expect(parseWorkMode("grid")).toStrictEqual({ mode: "grid", malformed: false });
    expect(parseWorkMode("tmux")).toStrictEqual({ mode: "tmux", malformed: false });
  });

  it("an unknown value is grid AND malformed — never tmux", () => {
    expect(parseWorkMode("TMUX")).toStrictEqual({ mode: "grid", malformed: true });
    expect(parseWorkMode("")).toStrictEqual({ mode: "grid", malformed: true });
    expect(parseWorkMode(1)).toStrictEqual({ mode: "grid", malformed: true });
    expect(parseWorkMode({ mode: "tmux" })).toStrictEqual({ mode: "grid", malformed: true });
  });
});

describe("pushMru", () => {
  it("adds to the head of an empty list", () => {
    expect(pushMru([], "a")).toStrictEqual(["a"]);
  });

  it("moves an existing entry to the head without duplicating it", () => {
    expect(pushMru(["a", "b", "c"], "c")).toStrictEqual(["c", "a", "b"]);
  });

  it("returns the same list when the tab is already the head", () => {
    const mru = ["a", "b"];
    expect(pushMru(mru, "a")).toBe(mru);
  });

  it("is bounded", () => {
    expect(pushMru(["a", "b", "c"], "d", 3)).toStrictEqual(["d", "a", "b"]);
    const full = Array.from({ length: MRU_LIMIT }, (_, i) => `t${i}`);
    const next = pushMru(full, "new");
    expect(next).toHaveLength(MRU_LIMIT);
    expect(next[0]).toBe("new");
    expect(next[MRU_LIMIT - 1]).toBe(`t${MRU_LIMIT - 2}`);
  });
});

describe("parseMru", () => {
  it("keeps strings, drops the rest, and reads a non-array as empty", () => {
    expect(parseMru(["a", 1, "b", null])).toStrictEqual(["a", "b"]);
    expect(parseMru(undefined)).toStrictEqual([]);
    expect(parseMru("a")).toStrictEqual([]);
  });
});

describe("nextShownAfterClose", () => {
  it("no tabs left → null", () => {
    expect(nextShownAfterClose(["a"], [], "a")).toBeNull();
  });

  it("the only survivor is shown", () => {
    expect(nextShownAfterClose(["a"], ["a", "b"], "a")).toBe("b");
  });

  it("the most recently shown survivor wins over creation order", () => {
    // Shown history a ← c ← b; closing a shows c, not b (b precedes it in creation order).
    expect(nextShownAfterClose(["a", "c", "b"], ["b", "c", "d", "a"], "a")).toBe("c");
  });

  it("MRU entries that are no longer live are skipped", () => {
    expect(nextShownAfterClose(["a", "gone", "d"], ["b", "d", "a"], "a")).toBe("d");
  });

  it("without MRU, falls back to the first live tab in creation order", () => {
    expect(nextShownAfterClose([], ["b", "c", "a"], "a")).toBe("b");
  });

  it("never returns the closed tab even while it is still listed live", () => {
    expect(nextShownAfterClose(["a"], ["a"], "a")).toBeNull();
  });

  it("with no closed tab, picks the MRU head", () => {
    expect(nextShownAfterClose(["c", "a"], ["a", "b", "c"], null)).toBe("c");
  });
});

describe("shownTabOnEnter", () => {
  it("is the focused zone's tab when it is live", () => {
    expect(shownTabOnEnter({ 0: "a", 2: "c" }, 2, ["a"], ["a", "c"])).toBe("c");
  });

  it("falls back to the MRU head when the focused zone is empty", () => {
    expect(shownTabOnEnter({ 0: "a", 2: "c" }, 1, ["c", "a"], ["a", "c"])).toBe("c");
  });

  it("falls back to creation order with no MRU, and to null with no tabs", () => {
    expect(shownTabOnEnter({}, 0, [], ["b", "a"])).toBe("b");
    expect(shownTabOnEnter({}, 0, [], [])).toBeNull();
  });
});

describe("reconcileTmuxAssignments", () => {
  it("0 tabs → empty, keeping identity when already empty", () => {
    const prev = {};
    expect(reconcileTmuxAssignments(prev, [], [])).toBe(prev);
    expect(reconcileTmuxAssignments({ 0: "a" }, [], ["a"])).toStrictEqual({});
  });

  it("1 tab fills the empty zone 0", () => {
    expect(reconcileTmuxAssignments({}, ["a"], [])).toStrictEqual({ 0: "a" });
  });

  it("a live shown tab is kept (same identity) however many others exist", () => {
    const prev = { 0: "b" };
    expect(reconcileTmuxAssignments(prev, ["a", "b"], ["b"])).toBe(prev);
    expect(reconcileTmuxAssignments(prev, ["a", "b", "c", "d", "e"], ["b"])).toBe(prev);
  });

  it("new tabs do not take zone 0 by creation order", () => {
    expect(reconcileTmuxAssignments({ 0: "c" }, ["a", "b", "c", "new"], ["c"])).toStrictEqual({
      0: "c",
    });
  });

  it("closing the shown tab shows the most recently shown survivor", () => {
    expect(reconcileTmuxAssignments({ 0: "c" }, ["a", "b", "d"], ["c", "d", "a"])).toStrictEqual({
      0: "d",
    });
  });

  it("closing the shown tab with no MRU falls back to creation order", () => {
    expect(reconcileTmuxAssignments({ 0: "c" }, ["a", "b"], [])).toStrictEqual({ 0: "a" });
  });

  it("closing a hidden tab leaves the shown one alone", () => {
    const prev = { 0: "a" };
    expect(reconcileTmuxAssignments(prev, ["a", "c"], ["a", "b"])).toBe(prev);
  });

  it("drops any zone other than 0", () => {
    expect(reconcileTmuxAssignments({ 0: "a", 3: "b" }, ["a", "b"], [])).toStrictEqual({ 0: "a" });
  });
});

describe("parseGridSnapshot", () => {
  it("reads a well-formed snapshot", () => {
    expect(
      parseGridSnapshot({ layoutId: "quad", assignments: { 0: "a", 3: "b" }, focusedZone: 3 }),
    ).toStrictEqual({ layoutId: "quad", assignments: { 0: "a", 3: "b" }, focusedZone: 3 });
  });

  it("anything malformed is null", () => {
    expect(parseGridSnapshot(undefined)).toBeNull();
    expect(parseGridSnapshot(null)).toBeNull();
    expect(parseGridSnapshot("quad")).toBeNull();
    expect(parseGridSnapshot({ layoutId: "quad", assignments: {} })).toBeNull();
    expect(
      parseGridSnapshot({ layoutId: "quad", assignments: { x: "a" }, focusedZone: 0 }),
    ).toBeNull();
    expect(
      parseGridSnapshot({ layoutId: "quad", assignments: { 0: 5 }, focusedZone: 0 }),
    ).toBeNull();
  });
});

describe("restoreGridSnapshot", () => {
  const snapshot = { layoutId: "quad", assignments: { 0: "a", 1: "b", 3: "d" }, focusedZone: 3 };

  it("every tab still live keeps its exact zone", () => {
    expect(restoreGridSnapshot(snapshot, ["a", "b", "d"], 4)).toStrictEqual(snapshot);
  });

  it("a tab closed during tmux leaves its zone empty (no re-pack)", () => {
    expect(restoreGridSnapshot(snapshot, ["a", "d"], 4)).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 3: "d" },
      focusedZone: 3,
    });
  });

  it("a tab created during tmux fills the lowest empty zone", () => {
    expect(restoreGridSnapshot(snapshot, ["a", "b", "d", "new"], 4)).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 1: "b", 2: "new", 3: "d" },
      focusedZone: 3,
    });
  });

  it("one created and one closed: the new tab takes the lowest hole", () => {
    expect(restoreGridSnapshot(snapshot, ["a", "d", "new"], 4)).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 1: "new", 3: "d" },
      focusedZone: 3,
    });
  });

  it("tabs that do not fit stay unassigned for auto-grow", () => {
    expect(restoreGridSnapshot(snapshot, ["a", "b", "d", "n1", "n2"], 4)).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 1: "b", 2: "n1", 3: "d" },
      focusedZone: 3,
    });
  });

  it("an out-of-range focused zone resets to 0", () => {
    expect(
      restoreGridSnapshot({ layoutId: "split", assignments: { 0: "a" }, focusedZone: 5 }, ["a"], 2),
    ).toStrictEqual({ layoutId: "split", assignments: { 0: "a" }, focusedZone: 0 });
  });
});

/**
 * The pure tmux work-mode rules (plan `2026-10-11-runner-terminal-tmux-work-mode`,
 * Phase 1). Every case asserts the literal resulting state.
 */

import { describe, expect, it } from "vitest";
import { MAX_WEBGL_PANES } from "./backends/webglContextLru";
import {
  MRU_LIMIT,
  TMUX_WARM_TAB_LIMIT,
  gridZoneOfTab,
  mruOnEnter,
  placeInGridSnapshot,
  rekeyGridSnapshot,
  rekeyMru,
  replaceTabInAssignments,
  replaceTabInGridSnapshot,
  replaceTabInMru,
  tabsBySessionKey,
  tmuxMountedTabIds,
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
  it("a live tab beats an exited one, from the MRU and from creation order alike", () => {
    const alive = new Set(["c"]);
    // MRU head b has exited: the live c wins although it is older in the MRU.
    expect(nextShownAfterClose(["a", "b", "c"], ["a", "b", "c"], "a", alive)).toBe("c");
    // No live tab in the MRU: the first live tab in creation order.
    expect(nextShownAfterClose(["a", "b"], ["b", "c", "a"], "a", alive)).toBe("c");
  });

  it("an exited tab is shown only when nothing live is left, MRU first", () => {
    const alive = new Set<string>();
    expect(nextShownAfterClose(["a", "c", "b"], ["b", "c", "a"], "a", alive)).toBe("c");
    expect(nextShownAfterClose([], ["b", "c"], null, alive)).toBe("b");
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

  it("the fallback prefers a live tab", () => {
    expect(shownTabOnEnter({}, 0, ["a"], ["a", "b"], new Set(["b"]))).toBe("b");
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

  it("closing the shown tab skips an exited tombstone for a live tab", () => {
    expect(
      reconcileTmuxAssignments({ 0: "c" }, ["a", "b"], ["c", "b", "a"], new Set(["a"])),
    ).toStrictEqual({ 0: "a" });
  });

  it("an exited shown tab is kept: the operator sees how it ended", () => {
    const prev = { 0: "a" };
    expect(reconcileTmuxAssignments(prev, ["a", "b"], ["a"], new Set(["b"]))).toBe(prev);
  });

  it("drops any zone other than 0", () => {
    expect(reconcileTmuxAssignments({ 0: "a", 3: "b" }, ["a", "b"], [])).toStrictEqual({ 0: "a" });
  });
});

describe("parseGridSnapshot", () => {
  const known = (id: string) => ["quad", "single"].includes(id);
  it("reads a well-formed snapshot", () => {
    expect(
      parseGridSnapshot(
        {
          layoutId: "quad",
          assignments: { 0: "a", 3: "b" },
          focusedZone: 3,
          sessionKeys: { 3: "sess-b" },
        },
        known,
      ),
    ).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 3: "b" },
      focusedZone: 3,
      sessionKeys: { 3: "sess-b" },
    });
  });

  it("a snapshot saved before session keys existed reads with none", () => {
    expect(
      parseGridSnapshot({ layoutId: "quad", assignments: { 0: "a" }, focusedZone: 0 }, known),
    ).toStrictEqual({ layoutId: "quad", assignments: { 0: "a" }, focusedZone: 0, sessionKeys: {} });
  });

  it("anything malformed is null", () => {
    expect(parseGridSnapshot(undefined, known)).toBeNull();
    expect(parseGridSnapshot(null, known)).toBeNull();
    expect(parseGridSnapshot("quad", known)).toBeNull();
    expect(parseGridSnapshot({ layoutId: "quad", assignments: {} }, known)).toBeNull();
    expect(
      parseGridSnapshot({ layoutId: "quad", assignments: { x: "a" }, focusedZone: 0 }, known),
    ).toBeNull();
    expect(
      parseGridSnapshot({ layoutId: "quad", assignments: { 0: 5 }, focusedZone: 0 }, known),
    ).toBeNull();
    expect(
      parseGridSnapshot(
        {
          layoutId: "quad",
          assignments: { 0: "a" },
          focusedZone: 0,
          sessionKeys: { 0: 7 },
        },
        known,
      ),
    ).toBeNull();
  });

  it("a layout id the page cannot lay out is malformed", () => {
    const snapshot = { layoutId: "bogus", assignments: { 0: "a" }, focusedZone: 0 };
    expect(parseGridSnapshot(snapshot, known)).toBeNull();
    expect(parseGridSnapshot({ ...snapshot, layoutId: 7 }, () => true)).toBeNull();
  });

  it("a focused zone that is not a non-negative integer is malformed", () => {
    for (const focusedZone of [-1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, "2"]) {
      expect(
        parseGridSnapshot({ layoutId: "quad", assignments: {}, focusedZone }, known),
      ).toBeNull();
    }
  });
});

describe("restoreGridSnapshot", () => {
  const snapshot = {
    layoutId: "quad",
    assignments: { 0: "a", 1: "b", 3: "d" },
    focusedZone: 3,
    sessionKeys: {},
  };

  it("every tab still live keeps its exact zone", () => {
    expect(restoreGridSnapshot(snapshot, ["a", "b", "d"], 4)).toStrictEqual(snapshot);
  });

  it("a tab closed during tmux leaves its zone empty (no re-pack)", () => {
    expect(restoreGridSnapshot(snapshot, ["a", "d"], 4)).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 3: "d" },
      focusedZone: 3,
      sessionKeys: {},
    });
  });

  it("a tab created during tmux fills the lowest empty zone", () => {
    expect(restoreGridSnapshot(snapshot, ["a", "b", "d", "new"], 4)).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 1: "b", 2: "new", 3: "d" },
      focusedZone: 3,
      sessionKeys: {},
    });
  });

  it("one created and one closed: the new tab takes the lowest hole", () => {
    expect(restoreGridSnapshot(snapshot, ["a", "d", "new"], 4)).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 1: "new", 3: "d" },
      focusedZone: 3,
      sessionKeys: {},
    });
  });

  it("tabs that do not fit stay unassigned for auto-grow", () => {
    expect(restoreGridSnapshot(snapshot, ["a", "b", "d", "n1", "n2"], 4)).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 1: "b", 2: "n1", 3: "d" },
      focusedZone: 3,
      sessionKeys: {},
    });
  });

  it("an out-of-range focused zone resets to 0", () => {
    expect(
      restoreGridSnapshot(
        { layoutId: "split", assignments: { 0: "a" }, focusedZone: 5, sessionKeys: {} },
        ["a"],
        2,
      ),
    ).toStrictEqual({
      layoutId: "split",
      assignments: { 0: "a" },
      focusedZone: 0,
      sessionKeys: {},
    });
  });

  it("sessions whose tabs were replaced keep their zones, matched by session key", () => {
    const saved = {
      layoutId: "quad",
      assignments: { 0: "a", 2: "b", 3: "shell" },
      focusedZone: 2,
      sessionKeys: { 0: "sess-a", 2: "sess-b" },
    };
    // After a runner restart: new tab ids, in an order unrelated to the zones;
    // the plain shell was not resumed.
    const keyOf = (id: string) => ({ b2: "sess-b", a2: "sess-a" })[id];
    expect(restoreGridSnapshot(saved, ["b2", "x", "a2"], 4, keyOf)).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a2", 1: "x", 2: "b2" },
      focusedZone: 2,
      sessionKeys: { 0: "sess-a", 2: "sess-b" },
    });
  });
});

describe("rekeyGridSnapshot", () => {
  const base = {
    layoutId: "quad",
    assignments: { 0: "a", 1: "b" },
    focusedZone: 0,
    sessionKeys: { 0: "sess-a" },
  };

  it("returns the snapshot itself when nothing moved", () => {
    expect(rekeyGridSnapshot(base, ["a", "b"], (id) => ({ a: "sess-a" })[id])).toBe(base);
  });

  it("records a key a zone's tab gained since", () => {
    const next = rekeyGridSnapshot(base, ["a", "b"], (id) => ({ a: "sess-a", b: "sess-b" })[id]);
    expect(next.sessionKeys).toStrictEqual({ 0: "sess-a", 1: "sess-b" });
  });

  it("re-points a zone at the tab its session now runs in", () => {
    const next = rekeyGridSnapshot(base, ["b", "a2"], (id) => ({ a2: "sess-a" })[id]);
    expect(next.assignments).toStrictEqual({ 0: "a2", 1: "b" });
  });

  it("never takes a tab that already holds another zone", () => {
    const snapshot = { ...base, assignments: { 0: "a", 1: "a2" } };
    const next = rekeyGridSnapshot(snapshot, ["a2"], (id) => ({ a2: "sess-a" })[id]);
    expect(next.assignments).toStrictEqual({ 0: "a", 1: "a2" });
  });

  it("leaves a zone whose session has not come back as it was", () => {
    expect(rekeyGridSnapshot(base, ["b"], () => undefined)).toBe(base);
  });

  it("re-points at the session's live tab, never its exited tombstone", () => {
    // a-dead (created first) is the exited tab; a2 is where the session runs now.
    const keyOf = (id: string) => ({ "a-dead": "sess-a", a2: "sess-a" })[id];
    const next = rekeyGridSnapshot(base, ["a-dead", "b", "a2"], keyOf, new Set(["b", "a2"]));
    expect(next.assignments).toStrictEqual({ 0: "a2", 1: "b" });
  });
});

describe("tabsBySessionKey", () => {
  const keyOf = (id: string) => ({ x1: "k", x2: "k", x3: "k", y: "j" })[id];

  it("prefers a live tab over an exited one, whatever their order", () => {
    expect(tabsBySessionKey(["x1", "x2", "y"], keyOf, new Set(["x2", "y"])).get("k")).toBe("x2");
  });

  it("breaks a tie between equally live tabs by creation order", () => {
    expect(tabsBySessionKey(["x1", "x2", "x3"], keyOf, new Set(["x2", "x3"])).get("k")).toBe("x2");
    expect(tabsBySessionKey(["x1", "x2"], keyOf, new Set()).get("k")).toBe("x1");
    expect(tabsBySessionKey(["x3", "x1"], keyOf).get("k")).toBe("x3");
  });
});

describe("rekeyMru", () => {
  it("returns both inputs as given when nothing moved", () => {
    const mru = ["a", "b"];
    const keys = { a: "sess-a" };
    const out = rekeyMru(mru, keys, ["a", "b"], (id) => ({ a: "sess-a" })[id]);
    expect(out.mru).toBe(mru);
    expect(out.keys).toBe(keys);
  });

  it("records the key of an entry whose tab gained one", () => {
    const out = rekeyMru(["a", "b"], {}, ["a", "b"], (id) => ({ b: "sess-b" })[id]);
    expect(out.keys).toStrictEqual({ b: "sess-b" });
  });

  it("re-points each gone entry at its session's new tab, keeping its place", () => {
    const keys = { a: "sess-a", b: "sess-b", c: "sess-c" };
    const keyOf = (id: string) => ({ c2: "sess-c", a2: "sess-a" })[id];
    expect(rekeyMru(["b", "a", "c"], keys, ["c2", "x", "a2"], keyOf)).toStrictEqual({
      mru: ["b", "a2", "c2"],
      keys: { b: "sess-b", a2: "sess-a", c2: "sess-c" },
    });
  });

  it("keeps a newer entry where a re-pointed one would duplicate it", () => {
    const out = rekeyMru(["a2", "a"], { a: "sess-a" }, ["a2"], (id) => ({ a2: "sess-a" })[id]);
    expect(out.mru).toStrictEqual(["a2"]);
  });

  it("re-points at the live tab, not an exited one with the same key", () => {
    const keyOf = (id: string) => ({ dead: "sess-a", live: "sess-a" })[id];
    const out = rekeyMru(["a"], { a: "sess-a" }, ["dead", "live"], keyOf, new Set(["live"]));
    expect(out.mru).toStrictEqual(["live"]);
  });
});

describe("replacing a tab in place", () => {
  it("assignments: the new tab takes the old one's zone, and leaves any other", () => {
    expect(replaceTabInAssignments({ 0: "x", 2: "old", 3: "new" }, "old", "new")).toStrictEqual({
      0: "x",
      2: "new",
    });
    const same = { 0: "a" };
    expect(replaceTabInAssignments(same, "old", "new")).toBe(same);
  });

  it("grid snapshot: same zone, the old session's key replaced by the new tab's", () => {
    const snapshot = {
      layoutId: "quad",
      assignments: { 0: "a", 2: "old" },
      focusedZone: 2,
      sessionKeys: { 0: "sess-a", 2: "sess-old" },
    };
    expect(replaceTabInGridSnapshot(snapshot, "old", "new", undefined)).toStrictEqual({
      ...snapshot,
      assignments: { 0: "a", 2: "new" },
      sessionKeys: { 0: "sess-a" },
    });
    expect(replaceTabInGridSnapshot(snapshot, "old", "new", "sess-new").sessionKeys).toStrictEqual({
      0: "sess-a",
      2: "sess-new",
    });
    expect(replaceTabInGridSnapshot(snapshot, "zzz", "new", undefined)).toBe(snapshot);
  });

  it("MRU: the new tab takes the old one's recency", () => {
    expect(replaceTabInMru(["new", "a", "old", "b"], "old", "new")).toStrictEqual([
      "new",
      "a",
      "b",
    ]);
    expect(replaceTabInMru(["a", "old"], "old", "new")).toStrictEqual(["a", "new"]);
    const same = ["a"];
    expect(replaceTabInMru(same, "old", "new")).toBe(same);
  });
});

describe("placeInGridSnapshot and gridZoneOfTab", () => {
  const base = {
    layoutId: "quad",
    assignments: { 0: "a", 1: "b" },
    focusedZone: 0,
    sessionKeys: { 0: "sess-a", 1: "sess-b" },
  };

  it("places a tab in a zone, moving it out of any other and dropping the zone's old key", () => {
    expect(placeInGridSnapshot(base, 0, "b")).toStrictEqual({
      ...base,
      assignments: { 0: "b" },
      sessionKeys: {},
    });
    expect(placeInGridSnapshot(base, 3, "r").assignments).toStrictEqual({ 0: "a", 1: "b", 3: "r" });
  });

  it("finds the zone a tab holds, or null", () => {
    expect(gridZoneOfTab(base, "b")).toBe(1);
    expect(gridZoneOfTab(base, "z")).toBeNull();
    expect(gridZoneOfTab(null, "b")).toBeNull();
    expect(gridZoneOfTab(base, null)).toBeNull();
  });
});

describe("mruOnEnter", () => {
  it("is the shown tab, then the grid's zones in order, then the old MRU", () => {
    expect(mruOnEnter("c", { 0: "a", 2: "c", 1: "b" }, ["z", "a"], ["a", "b", "c", "z"])).toEqual([
      "c",
      "a",
      "b",
      "z",
    ]);
  });

  it("drops ids that are not tabs any more", () => {
    expect(mruOnEnter("a", { 0: "a", 1: "gone" }, ["old"], ["a"])).toEqual(["a"]);
  });

  it("puts the tabs mounted at entry ahead of the other zones", () => {
    const assignments = { 0: "z0", 1: "z1", 2: "z2", 3: "z3" };
    const tabs = ["z0", "z1", "z2", "z3", "parked"];
    expect(mruOnEnter("z2", assignments, [], tabs, new Set(["z3", "z2", "parked"]))).toEqual([
      "z2",
      "z3",
      "parked",
      "z0",
      "z1",
    ]);
  });
});

describe("tmuxMountedTabIds", () => {
  const ptys = (ids: string[]) => ids.map((id) => ({ id }));

  it("is the shown tab plus the most recently shown others, up to the limit", () => {
    const tabs = ptys(["a", "b", "c", "d", "e"]);
    expect([...tmuxMountedTabIds("a", ["a", "c", "b", "e", "d"], tabs, 2)]).toEqual([
      "a",
      "c",
      "b",
    ]);
  });

  it("skips MRU entries that are no longer tabs", () => {
    expect([...tmuxMountedTabIds("a", ["gone", "b"], ptys(["a", "b"]), 1)]).toEqual(["a", "b"]);
  });

  it("shown plus warm fills the WebGL cap exactly", () => {
    const ids = Array.from({ length: 20 }, (_, i) => `t${i}`);
    expect(tmuxMountedTabIds("t0", ids, ptys(ids)).size).toBe(MAX_WEBGL_PANES);
    expect(TMUX_WARM_TAB_LIMIT).toBe(MAX_WEBGL_PANES - 1);
  });

  it("skips plan and worker tabs in the MRU rather than counting them warm", () => {
    const ids = Array.from({ length: 12 }, (_, i) => `t${i}`);
    const tabs = [
      ...ptys(ids),
      { id: "plan", type: "plan" },
      { id: "worker", sessionBacked: true },
    ];
    // The MRU names the plan and worker tabs among the most recent.
    const mru = ["t0", "plan", "t1", "worker", ...ids.slice(2)];
    const mounted = tmuxMountedTabIds("t0", mru, tabs);
    expect(mounted.size).toBe(MAX_WEBGL_PANES);
    expect(mounted.has("plan")).toBe(false);
    expect(mounted.has("worker")).toBe(false);
    expect([...mounted]).toEqual(ids.slice(0, MAX_WEBGL_PANES));
  });

  it("skips a restore placeholder in the MRU — no PTY to keep warm", () => {
    const ids = Array.from({ length: 12 }, (_, i) => `t${i}`);
    const tabs = [...ptys(ids), { id: "ph", type: "restore-placeholder" }];
    const mounted = tmuxMountedTabIds("t0", ["t0", "ph", ...ids.slice(1)], tabs);
    expect(mounted.has("ph")).toBe(false);
    expect([...mounted]).toEqual(ids.slice(0, MAX_WEBGL_PANES));
  });
});

/**
 * `useZoneLayout` in tmux work mode (plan `2026-10-11-runner-terminal-tmux-work-mode`,
 * Phase 1), driven through the hook itself so the ordering of the reconcile,
 * MRU and auto-grow effects against the mode transitions is what is tested.
 *
 * The invariant being switched: in grid mode every live tab gets a zone; in
 * tmux mode exactly one tab is on screen (zone 0 of a pinned `single`
 * layout), the rest stay live off-screen, and only an OPERATOR layout request
 * leaves the mode.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", async () => {
  const { hooksHarness } = await import("@/lib/__test-helpers__/hooks-harness");
  return hooksHarness.reactMock;
});

const storage = vi.hoisted(() => ({ value: null as unknown, written: [] as unknown[] }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: () => storage.value,
    setJSON: (_key: string, value: unknown) => {
      storage.written.push(value);
    },
  },
}));

import { hooksHarness as harness } from "@/lib/__test-helpers__/hooks-harness";
import { useZoneLayout } from "./useZoneLayout";

/** What the page threads besides the tab ids: liveness and session keys. */
interface TabFacts {
  /** Tab ids whose process is alive; absent = every tab is alive. */
  alive?: string[];
  /** tab id → claudeSessionId. */
  keys?: Record<string, string>;
}

/**
 * Mount on a persisted state; the returned function re-renders, optionally
 * with new tabs (and new liveness / session keys).
 */
function mount(persisted: unknown, tabIds: string[], initialFacts: TabFacts = {}) {
  harness.reset();
  storage.value = persisted;
  storage.written = [];
  let live = tabIds;
  // Built once per change, as the page memoizes them on its tab list.
  const toInputs = (f: TabFacts) => ({
    alive: f.alive ? new Set(f.alive) : undefined,
    keys: f.keys ? new Map(Object.entries(f.keys)) : undefined,
  });
  let facts = toInputs(initialFacts);
  function ZoneLayoutHost() {
    harness.beginRender();
    return useZoneLayout(live, "tmux-test", "main", facts.alive, facts.keys);
  }
  return (next?: string[], nextFacts?: TabFacts) => {
    if (next) live = next;
    if (nextFacts) facts = toInputs(nextFacts);
    return harness.renderSettled(ZoneLayoutHost);
  };
}

const grid = (layoutId: string, assignments: Record<number, string>, focusedZone = 0) => ({
  layoutId,
  assignments,
  focusedZone,
});

/** The tmux-relevant slice of the hook's result. */
function view(r: ReturnType<ReturnType<typeof mount>>) {
  return {
    workMode: r.workMode,
    layoutId: r.layoutId,
    assignments: r.assignments,
    shownTabId: r.shownTabId,
    mru: r.mru,
  };
}

let warn: ReturnType<typeof vi.spyOn>;
beforeEach(() => {
  harness.reset();
  warn = vi.spyOn(console, "warn").mockImplementation(() => {});
});
afterEach(() => warn.mockRestore());

describe("persisted work mode", () => {
  it("absent → grid, nothing logged", () => {
    const r = mount(grid("quad", { 0: "a", 1: "b" }), ["a", "b"])();
    expect(r.workMode).toBe("grid");
    expect(r.layoutId).toBe("quad");
    expect(warn).not.toHaveBeenCalled();
  });

  it("malformed → grid, with a warning naming the value", () => {
    const r = mount({ ...grid("quad", { 0: "a", 1: "b" }), workMode: "TMUX" }, ["a", "b"])();
    expect(view(r)).toStrictEqual({
      workMode: "grid",
      layoutId: "quad",
      assignments: { 0: "a", 1: "b" },
      shownTabId: null,
      mru: [],
    });
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0][0])).toContain('"TMUX"');
  });

  it("tmux reloads pinned to single even when another layout id was stored", () => {
    const r = mount(
      { ...grid("quad", { 0: "b" }, 2), workMode: "tmux", mru: ["b"], gridSnapshot: null },
      ["a", "b", "c"],
    )();
    expect(view(r)).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "b" },
      shownTabId: "b",
      mru: ["b"],
    });
    expect(r.focusedZone).toBe(0);
  });

  it("mode, MRU and snapshot are written alongside the layout", () => {
    const render = mount(grid("split", { 0: "a", 1: "b" }, 1), ["a", "b"]);
    render().enterTmuxMode();
    render();
    expect(storage.written.at(-1)).toStrictEqual({
      layoutId: "single",
      assignments: { 0: "b" },
      focusedZone: 0,
      workMode: "tmux",
      mru: ["b", "a"],
      gridSnapshot: {
        layoutId: "split",
        assignments: { 0: "a", 1: "b" },
        focusedZone: 1,
        sessionKeys: {},
      },
    });
  });
});

describe("entering tmux mode — 0 / 1 / 2 / many tabs", () => {
  it("0 tabs: pinned single, nothing shown", () => {
    const render = mount(null, []);
    render().enterTmuxMode();
    expect(view(render())).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: {},
      shownTabId: null,
      mru: [],
    });
  });

  it("0 tabs, then the first tab arrives: it is shown", () => {
    const render = mount(null, []);
    render().enterTmuxMode();
    render();
    expect(view(render(["a"]))).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "a" },
      shownTabId: "a",
      mru: ["a"],
    });
  });

  it("1 tab: it is shown", () => {
    const render = mount(grid("single", { 0: "a" }), ["a"]);
    render().enterTmuxMode();
    expect(view(render())).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "a" },
      shownTabId: "a",
      mru: ["a"],
    });
  });

  it("2 tabs: the focused zone's tab is shown, maximize cleared", () => {
    const render = mount(grid("split", { 0: "a", 1: "b" }, 1), ["a", "b"]);
    const before = render();
    before.toggleMaximize(1);
    expect(render().maximizedZone).toBe(1);
    render().enterTmuxMode();
    const r = render();
    expect(view(r)).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "b" },
      shownTabId: "b",
      // The grid's other session follows the shown one: it was on screen a
      // moment ago, so it is the most recently seen.
      mru: ["b", "a"],
    });
    expect(r.maximizedZone).toBeNull();
    expect(r.isMultiZone).toBe(false);
    expect(r.unassignedTabIds).toEqual(["a"]);
  });

  it("many tabs: no auto-grow, ever — adding more tabs keeps single and the shown tab", () => {
    const six = ["a", "b", "c", "d", "e", "f"];
    const render = mount(
      grid("six-pack", { 0: "a", 1: "b", 2: "c", 3: "d", 4: "e", 5: "f" }, 3),
      six,
    );
    render().enterTmuxMode();
    expect(view(render())).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "d" },
      shownTabId: "d",
      mru: ["d", "a", "b", "c", "e", "f"],
    });
    // Background ingest (not an operator spawn) does not take the screen.
    const more = [...six, "g", "h", "i", "j", "k"];
    expect(view(render(more))).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "d" },
      shownTabId: "d",
      mru: ["d", "a", "b", "c", "e", "f"],
    });
  });

  it("an empty focused zone falls back to the MRU head", () => {
    const render = mount({ ...grid("quad", { 0: "a", 1: "b", 3: "c" }, 2), mru: ["c", "a"] }, [
      "a",
      "b",
      "c",
    ]);
    render().enterTmuxMode();
    expect(render().shownTabId).toBe("c");
  });
});

describe("swaps, closes and the MRU", () => {
  function inTmux(tabs: string[]) {
    const render = mount(grid("quad", { 0: tabs[0] }), tabs);
    render().enterTmuxMode();
    render();
    return render;
  }

  it("entering seeds the MRU with the shown tab, then the grid's zones in order", () => {
    // The quad's reconcile zoned b, c and d before the switch.
    expect(inTmux(["a", "b", "c", "d"])().mru).toStrictEqual(["a", "b", "c", "d"]);
  });

  it("each swap into zone 0 is recorded newest-first", () => {
    const render = inTmux(["a", "b", "c", "d"]);
    render().assignTabToZone(0, "c");
    render().assignTabToZone(0, "b");
    expect(view(render())).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "b" },
      shownTabId: "b",
      mru: ["b", "c", "a", "d"],
    });
  });

  it("closing the shown tab shows the most recently shown survivor", () => {
    const render = inTmux(["a", "b", "c", "d"]);
    render().assignTabToZone(0, "c");
    render().assignTabToZone(0, "b");
    expect(view(render(["a", "c", "d"]))).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "c" },
      shownTabId: "c",
      mru: ["c", "b", "a", "d"],
    });
  });

  it("closing the shown tab with no other MRU entry falls back to creation order", () => {
    const render = mount({ ...grid("single", { 0: "c" }), workMode: "tmux", mru: [] }, [
      "a",
      "b",
      "c",
    ]);
    expect(render().mru).toStrictEqual(["c"]);
    expect(view(render(["a", "b"]))).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "a" },
      shownTabId: "a",
      mru: ["a", "c"],
    });
  });

  it("closing a hidden tab leaves the shown tab and the MRU alone", () => {
    const render = inTmux(["a", "b", "c"]);
    render().assignTabToZone(0, "b");
    expect(view(render(["a", "b"]))).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "b" },
      shownTabId: "b",
      mru: ["b", "a", "c"],
    });
  });

  it("closing the last tab leaves nothing shown", () => {
    const render = inTmux(["a"]);
    expect(view(render([]))).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: {},
      shownTabId: null,
      mru: ["a"],
    });
  });

  it("an assignment to any zone but 0 is a no-op (the tab stays off-screen)", () => {
    const render = inTmux(["a", "b"]);
    render().assignTabToZone(2, "b");
    expect(render().assignments).toStrictEqual({ 0: "a" });
  });

  it("maximize is a no-op and reads null", () => {
    const render = inTmux(["a", "b"]);
    render().toggleMaximize(0);
    render().setMaximizedZone(0);
    expect(render().maximizedZone).toBeNull();
  });
});

describe("layout requests by source", () => {
  function inTmuxFromSplit() {
    const render = mount(grid("split", { 0: "a", 1: "b" }), ["a", "b"]);
    render().enterTmuxMode();
    render();
    return render;
  }

  it("spawn in tmux stays in tmux, and the spawned tab is shown", () => {
    const render = inTmuxFromSplit();
    const r = render(["a", "b", "n"]);
    r.setLayoutId("quad", { source: "spawn" });
    r.showSpawnedTab("n");
    expect(view(render())).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "n" },
      shownTabId: "n",
      mru: ["n", "a", "b"],
    });
  });

  it("restore in tmux stays in tmux and changes nothing", () => {
    const render = inTmuxFromSplit();
    render().setLayoutId("quad", { source: "restore" });
    expect(view(render())).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "a" },
      shownTabId: "a",
      mru: ["a", "b"],
    });
  });

  it("an operator preset pick exits tmux and applies the preset over the restored grid", () => {
    const render = mount(grid("quad", { 0: "a", 1: "b", 2: "c", 3: "d" }, 2), ["a", "b", "c", "d"]);
    render().enterTmuxMode();
    render().setLayoutId("six-pack", { source: "operator" });
    const r = render();
    expect(view(r)).toStrictEqual({
      workMode: "grid",
      layoutId: "six-pack",
      assignments: { 0: "a", 1: "b", 2: "c", 3: "d" },
      shownTabId: null,
      mru: ["c", "a", "b", "d"],
    });
    expect(r.focusedZone).toBe(2);
  });

  it("an operator pick too small for the tabs exits, then grid auto-grow fits them", () => {
    const render = mount(grid("quad", { 0: "a", 1: "b", 2: "c", 3: "d" }), ["a", "b", "c", "d"]);
    render().enterTmuxMode();
    render().setLayoutId("single", { source: "operator" });
    const r = render();
    expect(r.workMode).toBe("grid");
    expect(r.layoutId).toBe("quad");
    expect(r.unassignedTabIds).toEqual([]);
  });

  it("an unknown layout id is refused before it can exit tmux", () => {
    const render = inTmuxFromSplit();
    render().setLayoutId("nope", { source: "operator" });
    expect(render().workMode).toBe("tmux");
  });

  it("grid mode is unchanged: every source applies, showSpawnedTab is a no-op", () => {
    const render = mount(grid("split", { 0: "a", 1: "b" }), ["a", "b"]);
    render().setLayoutId("quad", { source: "spawn" });
    expect(render().layoutId).toBe("quad");
    render().setLayoutId("triptych", { source: "restore" });
    expect(render().layoutId).toBe("triptych");
    const r = render(["a", "b", "n"]);
    r.showSpawnedTab("n");
    expect(view(render())).toStrictEqual({
      workMode: "grid",
      layoutId: "triptych",
      assignments: { 0: "a", 1: "b", 2: "n" },
      shownTabId: null,
      mru: [],
    });
  });
});

describe("enter / exit round trip", () => {
  it("restores the grid the operator left, with one tab created and one closed during tmux", () => {
    const render = mount(grid("quad", { 0: "a", 1: "b", 2: "c", 3: "d" }, 2), ["a", "b", "c", "d"]);
    render().enterTmuxMode();
    expect(render().shownTabId).toBe("c");

    // Operator spawns `e` (shown), then `b` closes off-screen.
    render(["a", "b", "c", "d", "e"]).showSpawnedTab("e");
    expect(render(["a", "c", "d", "e"]).shownTabId).toBe("e");

    render().exitTmuxMode();
    const r = render();
    expect(view(r)).toStrictEqual({
      workMode: "grid",
      layoutId: "quad",
      assignments: { 0: "a", 1: "e", 2: "c", 3: "d" },
      shownTabId: null,
      mru: ["e", "c", "a", "b", "d"],
    });
    expect(r.focusedZone).toBe(2);
  });

  it("a tab created during tmux that does not fit makes the grid grow", () => {
    const render = mount(grid("split", { 0: "a", 1: "b" }), ["a", "b"]);
    render().enterTmuxMode();
    render(["a", "b", "c"]);
    render().exitTmuxMode();
    expect(view(render())).toStrictEqual({
      workMode: "grid",
      layoutId: "quad",
      assignments: { 0: "a", 1: "b", 2: "c" },
      shownTabId: null,
      mru: ["a", "b"],
    });
  });

  it("with no snapshot, exits to pickLayout(tabCount) keeping the shown tab in zone 0", () => {
    const render = mount({ ...grid("single", { 0: "b" }), workMode: "tmux", mru: ["b"] }, [
      "a",
      "b",
      "c",
    ]);
    render().exitTmuxMode();
    const r = render();
    expect(view(r)).toStrictEqual({
      workMode: "grid",
      layoutId: "quad",
      assignments: { 0: "b", 1: "a", 2: "c" },
      shownTabId: null,
      mru: ["b"],
    });
    expect(r.focusedZone).toBe(0);
  });

  it("enter is idempotent: a second enter does not overwrite the snapshot", () => {
    const render = mount(grid("quad", { 0: "a", 1: "b" }, 1), ["a", "b"]);
    render().enterTmuxMode();
    render().enterTmuxMode();
    render().exitTmuxMode();
    const r = render();
    expect(r.layoutId).toBe("quad");
    expect(r.assignments).toStrictEqual({ 0: "a", 1: "b" });
    expect(r.focusedZone).toBe(1);
  });

  it("exit in grid mode is a no-op", () => {
    const render = mount(grid("quad", { 0: "a", 1: "b" }, 1), ["a", "b"]);
    render().exitTmuxMode();
    expect(view(render())).toStrictEqual({
      workMode: "grid",
      layoutId: "quad",
      assignments: { 0: "a", 1: "b" },
      shownTabId: null,
      mru: [],
    });
  });

  it("a keep-AI compaction requested in tmux re-deals the snapshot, not the screen", () => {
    const render = mount(grid("full-grid", { 0: "s1", 4: "a1", 8: "a2" }, 4), ["s1", "a1", "a2"]);
    render().enterTmuxMode();
    expect(render().shownTabId).toBe("a1");
    render().requestCompaction(["a1", "a2"], ["s1"]);
    expect(view(render(["a1", "a2"]))).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "a1" },
      shownTabId: "a1",
      mru: ["a1", "s1", "a2"],
    });
    render().exitTmuxMode();
    const r = render();
    expect(r.layoutId).toBe("split");
    expect(r.assignments).toStrictEqual({ 0: "a1", 1: "a2" });
    expect(r.focusedZone).toBe(0);
  });
});

describe("exited tabs are shown only when no live tab is left", () => {
  it("closing the shown tab skips an exited MRU entry for a live tab", () => {
    const render = mount(grid("quad", { 0: "a", 1: "b", 2: "c" }), ["a", "b", "c"], {
      alive: ["a", "b", "c"],
    });
    render().enterTmuxMode();
    render().assignTabToZone(0, "b");
    render().assignTabToZone(0, "c");
    // MRU is c, b, a; b has exited and stays as a tombstone tab.
    render(undefined, { alive: ["a", "c"] });
    expect(render(["a", "b"], { alive: ["a"] }).shownTabId).toBe("a");
  });

  it("falls back to an exited tab when nothing live is left", () => {
    const render = mount(grid("split", { 0: "a", 1: "b" }), ["a", "b"], { alive: ["a", "b"] });
    render().enterTmuxMode();
    render(undefined, { alive: ["a"] });
    expect(render(["b"], { alive: [] }).shownTabId).toBe("b");
  });

  it("an exited shown tab stays on screen as its tombstone", () => {
    const render = mount(grid("split", { 0: "a", 1: "b" }), ["a", "b"], { alive: ["a", "b"] });
    render().enterTmuxMode();
    expect(render(undefined, { alive: ["b"] }).shownTabId).toBe("a");
  });
});

describe("the grid snapshot survives a runner restart", () => {
  it("re-keys zones by session id when the sessions come back under new tab ids", () => {
    const keys = { a: "sess-a", b: "sess-b", c: "sess-c" };
    const render = mount(grid("quad", { 0: "a", 1: "b", 3: "c" }, 3), ["a", "b", "c"], { keys });
    render().enterTmuxMode();
    // The runner restarts: every tab is gone, the sessions cold-resume into
    // new tabs in creation order — which is not their zone order.
    const r = render(["c2", "a2", "b2"], { keys: { c2: "sess-c", a2: "sess-a", b2: "sess-b" } });
    expect(r.gridView.assignments).toStrictEqual({ 0: "a2", 1: "b2", 3: "c2" });
    r.exitTmuxMode();
    const after = render();
    expect(after.layoutId).toBe("quad");
    expect(after.assignments).toStrictEqual({ 0: "a2", 1: "b2", 3: "c2" });
    expect(after.focusedZone).toBe(3);
  });

  it("records a session key bound after entering, so that session re-keys too", () => {
    const render = mount(grid("split", { 0: "a", 1: "b" }), ["a", "b"]);
    render().enterTmuxMode();
    render(undefined, { keys: { b: "sess-b" } });
    const r = render(["a", "b2"], { keys: { b2: "sess-b" } });
    expect(r.gridView.assignments).toStrictEqual({ 0: "a", 1: "b2" });
  });

  it("a restore in tmux places the session in the snapshot, never on screen", () => {
    const render = mount(grid("quad", { 0: "a", 1: "b" }), ["a", "b"]);
    render().enterTmuxMode();
    const r = render(["a", "b", "r"]);
    r.placeRestoredTab(0, "r");
    const after = render();
    expect(after.shownTabId).toBe("a");
    expect(after.gridView.assignments).toStrictEqual({ 0: "r", 1: "b" });
  });

  it("a restore in grid mode is a zone assignment", () => {
    const render = mount(grid("quad", { 0: "a" }), ["a", "r"]);
    render().placeRestoredTab(3, "r");
    expect(render().assignments).toStrictEqual({ 0: "a", 3: "r" });
  });

  it("a focus request for a zone tmux does not show is dropped", () => {
    const render = mount(grid("quad", { 0: "a", 1: "b" }, 1), ["a", "b"]);
    render().enterTmuxMode();
    render().setFocusedZone(3);
    const r = render();
    expect(r.focusedZone).toBe(0);
    expect(r.gridView.focusedZone).toBe(1);
  });
});

describe("gridView and cosmeticZoneFor", () => {
  it("grid mode: the live grid, and every zone is its own cosmetic zone", () => {
    const r = mount(grid("quad", { 0: "a", 1: "b" }, 1), ["a", "b"])();
    expect(r.gridView).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 1: "b" },
      focusedZone: 1,
    });
    expect(r.cosmeticZoneFor(1)).toBe(1);
  });

  it("tmux: the snapshot, and the viewport's cosmetics are the shown session's grid zone", () => {
    const render = mount(grid("quad", { 0: "a", 1: "b", 2: "c" }, 2), ["a", "b", "c"]);
    render().enterTmuxMode();
    let r = render();
    expect(r.gridView).toStrictEqual({
      layoutId: "quad",
      assignments: { 0: "a", 1: "b", 2: "c" },
      focusedZone: 2,
    });
    expect(r.cosmeticZoneFor(0)).toBe(2);
    r.assignTabToZone(0, "b");
    r = render();
    expect(r.cosmeticZoneFor(0)).toBe(1);
  });

  it("tmux: a session created during tmux has no grid zone, so no cosmetics", () => {
    const render = mount(grid("split", { 0: "a", 1: "b" }), ["a", "b"]);
    render().enterTmuxMode();
    render(["a", "b", "n"]).showSpawnedTab("n");
    expect(render().cosmeticZoneFor(0)).toBeNull();
  });
});

describe("keyboard focus on exit", () => {
  it("exiting asks for the restored focused zone's tab to take the keyboard", () => {
    const render = mount(grid("quad", { 0: "a", 1: "b", 2: "c" }, 1), ["a", "b", "c"]);
    render().enterTmuxMode();
    render().assignTabToZone(0, "c");
    const before = render().revealRequest?.seq ?? 0;
    render().exitTmuxMode();
    const r = render();
    expect(r.focusedZone).toBe(1);
    expect(r.revealRequest).toStrictEqual({ tabId: "b", seq: before + 1 });
  });
});

describe("a malformed persisted snapshot", () => {
  it("is ignored with a warning naming it", () => {
    const r = mount(
      {
        ...grid("single", { 0: "a" }),
        workMode: "tmux",
        gridSnapshot: { layoutId: "quad", assignments: { 0: "a" }, focusedZone: 1.5 },
      },
      ["a"],
    )();
    expect(r.gridView.layoutId).toBe("single");
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0][0])).toContain("gridSnapshot");
  });
});

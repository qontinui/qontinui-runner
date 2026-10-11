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

/** Mount on a persisted state; the returned function re-renders, optionally with new tabs. */
function mount(persisted: unknown, tabIds: string[]) {
  harness.reset();
  storage.value = persisted;
  storage.written = [];
  let live = tabIds;
  function ZoneLayoutHost() {
    harness.beginRender();
    return useZoneLayout(live, "tmux-test", "main");
  }
  return (next?: string[]) => {
    if (next) live = next;
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
      mru: ["b"],
      gridSnapshot: { layoutId: "split", assignments: { 0: "a", 1: "b" }, focusedZone: 1 },
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
      mru: ["b"],
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
      mru: ["d"],
    });
    // Background ingest (not an operator spawn) does not take the screen.
    const more = [...six, "g", "h", "i", "j", "k"];
    expect(view(render(more))).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "d" },
      shownTabId: "d",
      mru: ["d"],
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

  it("each swap into zone 0 is recorded newest-first", () => {
    const render = inTmux(["a", "b", "c", "d"]);
    render().assignTabToZone(0, "c");
    render().assignTabToZone(0, "b");
    expect(view(render())).toStrictEqual({
      workMode: "tmux",
      layoutId: "single",
      assignments: { 0: "b" },
      shownTabId: "b",
      mru: ["b", "c", "a"],
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
      mru: ["c", "b", "a"],
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
      mru: ["b", "a"],
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
      mru: ["n", "a"],
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
      mru: ["a"],
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
      mru: ["c"],
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
      mru: ["e", "c"],
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
      mru: ["a"],
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
      mru: ["a1"],
    });
    render().exitTmuxMode();
    const r = render();
    expect(r.layoutId).toBe("split");
    expect(r.assignments).toStrictEqual({ 0: "a1", 1: "a2" });
    expect(r.focusedZone).toBe(0);
  });
});

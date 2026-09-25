/**
 * Regression tests: closing a terminal in the flow-grid made ANOTHER terminal
 * disappear without closing.
 *
 * The flow-grid synthesized exactly one zone per live tab, so a close shrank
 * the grid by one. `reconcileAssignments` dropped the closed tab's assignment
 * and nothing else, so the tab in the LAST zone kept an index the grid no
 * longer rendered. `classifyTabs` still counted it as assigned, so it got
 * neither a zone nor the hidden mount. Its PTY stayed alive and it was drawn
 * nowhere, which is "disappears but doesn't close". Any close except the last
 * tile's did it.
 *
 * The hook-level cases drive `useZoneLayout` itself (through the shared hooks
 * harness), so they fail if the hook stops sizing the grid through
 * `flowGridSlotCount`, not only if the helper changes.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("react", async () => {
  const { hooksHarness } = await import("@/lib/__test-helpers__/hooks-harness");
  return hooksHarness.reactMock;
});

const persisted = vi.hoisted(() => ({ value: null as unknown }));
vi.mock("@/lib/instance-storage", () => ({
  instanceStorage: {
    getJSON: () => persisted.value,
    setJSON: () => {},
  },
}));

import { hooksHarness as harness } from "@/lib/__test-helpers__/hooks-harness";
import {
  flowGridSlotCount,
  useZoneLayout,
  FLOW_GRID_ID,
  type ZoneAssignments,
} from "./useZoneLayout";

const ids = (n: number) => Array.from({ length: n }, (_, i) => `t${i}`);

/** Dense assignments: tab `ti` in zone `i`. */
function dense(tabIds: string[]): ZoneAssignments {
  return Object.fromEntries(tabIds.map((id, i) => [i, id]));
}

/**
 * Mount `useZoneLayout` on a persisted layout and return a settled-render
 * function. Passing `tabIds` to it swaps the live tab list (a new array, as a
 * close or a spawn produces), so the hook's reconcile effect sees the change.
 */
function mount(layoutId: string, tabIds: string[], assignments: ZoneAssignments) {
  harness.reset();
  persisted.value = { layoutId, assignments, focusedZone: 0 };
  let live = tabIds;
  function ZoneLayoutHost() {
    harness.beginRender();
    return useZoneLayout(live, "flow-close-test", "main");
  }
  return (next?: string[]) => {
    if (next) live = next;
    return harness.renderSettled(ZoneLayoutHost);
  };
}

type Rendered = ReturnType<ReturnType<typeof mount>>;

/** Live tabs assigned to a zone the grid does not render: drawn nowhere. */
function drawnNowhere(r: Rendered, tabIds: string[]): string[] {
  return Object.entries(r.assignments)
    .filter(([zone, id]) => tabIds.includes(id) && Number(zone) >= r.layout.zones.length)
    .map(([, id]) => id);
}

const without = (tabIds: string[], ...closed: string[]) =>
  tabIds.filter((id) => !closed.includes(id));

describe("useZoneLayout — closing a flow-grid terminal hides no other terminal", () => {
  beforeEach(() => harness.reset());

  it("closing a middle tile keeps the last tile in a rendered zone", () => {
    const before = ids(10);
    const render = mount(FLOW_GRID_ID, before, dense(before));
    expect(render().layout.zones).toHaveLength(10);

    const after = without(before, "t3");
    const r = render(after);

    // The old sizing (zones == tab count) rendered 9 zones here, leaving t9
    // assigned to zone 9 and drawn nowhere.
    expect(drawnNowhere(r, after)).toEqual([]);
    expect(r.unassignedTabIds).toEqual([]);
    expect(r.layout.zones).toHaveLength(10);
    // Nothing is re-packed: every survivor keeps its zone, the close leaves
    // an empty tile.
    const expected = dense(before);
    delete expected[3];
    expect(r.assignments).toStrictEqual(expected);
  });

  it("closing ANY one of 12 tiles leaves every other tile drawn", () => {
    const before = ids(12);
    for (const closed of before) {
      const render = mount(FLOW_GRID_ID, before, dense(before));
      render();
      const after = without(before, closed);
      const r = render(after);
      expect(drawnNowhere(r, after)).toEqual([]);
      expect(r.unassignedTabIds).toEqual([]);
      for (const id of after) expect(r.assignments[Number(id.slice(1))]).toBe(id);
    }
  });

  it("the next new terminal fills the tile the close emptied", () => {
    const before = ids(10);
    const render = mount(FLOW_GRID_ID, before, dense(before));
    render();
    const closed = without(before, "t3");
    render(closed);

    const r = render([...closed, "t10"]);
    expect(r.assignments[3]).toBe("t10");
    expect(r.layout.zones).toHaveLength(10);
    expect(drawnNowhere(r, [...closed, "t10"])).toEqual([]);
  });

  it("the grid shrinks when the highest tiles close", () => {
    const before = ids(10);
    const render = mount(FLOW_GRID_ID, before, dense(before));
    render();
    expect(render(without(before, "t9")).layout.zones).toHaveLength(9);
    // A middle close keeps the grid at the highest occupied zone...
    expect(render(without(before, "t9", "t4")).layout.zones).toHaveLength(9);
    // ...and closing that highest tile then shrinks it.
    const after = without(before, "t9", "t4", "t8");
    const r = render(after);
    expect(r.layout.zones).toHaveLength(8);
    expect(drawnNowhere(r, after)).toEqual([]);
  });

  it("a restore that skipped a record keeps the tab past the gap drawn", () => {
    // Nine tabs came back; the record for zone 8 was skipped, so t9 holds
    // zone 9. The old sizing (9 zones) hid t9 from the first render.
    const tabIds = [...ids(8), "t9"];
    const assignments: ZoneAssignments = { ...dense(ids(8)), 9: "t9" };
    const r = mount(FLOW_GRID_ID, tabIds, assignments)();
    expect(r.layout.zones).toHaveLength(10);
    expect(r.assignments).toEqual(assignments);
    expect(drawnNowhere(r, tabIds)).toEqual([]);
  });

  it("preset layouts are unchanged: a close leaves its zone empty and the size fixed", () => {
    const before = ["a", "b", "c", "d"];
    const render = mount("quad", before, dense(before));
    render();
    const r = render(without(before, "b"));
    expect(r.layout.id).toBe("quad");
    expect(r.layout.zones).toHaveLength(4);
    expect(r.assignments).toEqual({ 0: "a", 2: "c", 3: "d" });
  });
});

describe("useZoneLayout — closing the maximized terminal returns to the grid", () => {
  beforeEach(() => harness.reset());

  it("un-maximizes when the maximized tile closes, rather than showing a blank page", () => {
    const before = ids(10);
    const render = mount(FLOW_GRID_ID, before, dense(before));
    render().setMaximizedZone(3);
    expect(render().maximizedZone).toBe(3);

    expect(render(without(before, "t3")).maximizedZone).toBeNull();
  });

  it("stays maximized when a DIFFERENT tile closes", () => {
    const before = ids(10);
    const render = mount(FLOW_GRID_ID, before, dense(before));
    render().setMaximizedZone(3);
    const r = render(without(before, "t5"));
    expect(r.maximizedZone).toBe(3);
    expect(r.assignments[3]).toBe("t3");
  });

  it("leaves an empty zone that was maximized on purpose alone", () => {
    // The UI Bridge `maximize-zone` action accepts any in-range zone and
    // reports the zone it set, so un-maximizing it would make that report lie.
    const render = mount("quad", ["a", "b"], { 0: "a", 1: "b" });
    render().setMaximizedZone(3);
    expect(render().maximizedZone).toBe(3);
    expect(render(["a", "b", "c"]).maximizedZone).toBe(3);
  });

  it("stays maximized when the shown tab is dragged out rather than closed", () => {
    const render = mount("quad", ["a", "b"], { 0: "a", 1: "b" });
    render().setMaximizedZone(0);
    render().assignTabToZone(2, "a");
    const r = render();
    expect(r.assignments[0]).toBeUndefined();
    expect(r.maximizedZone).toBe(0);
  });
});

describe("flowGridSlotCount", () => {
  it("is the tab count when assignments are dense", () => {
    expect(flowGridSlotCount(ids(10), dense(ids(10)))).toBe(10);
  });

  it("reaches the highest assigned zone", () => {
    expect(flowGridSlotCount(without(ids(10), "t3"), dense(ids(10)))).toBe(10);
  });

  it("ignores a dead assignment, so a closing tab cannot hold the grid open", () => {
    // The render right after closing t9 (the last tile): its assignment is
    // still present until the reconcile effect drops it.
    expect(flowGridSlotCount(ids(9), dense(ids(10)))).toBe(9);
  });

  it("covers unassigned tabs as well as assigned ones", () => {
    expect(flowGridSlotCount(ids(5), { 0: "t0" })).toBe(5);
  });
});

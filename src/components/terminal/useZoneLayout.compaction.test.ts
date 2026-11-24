/**
 * `useZoneLayout.requestCompaction` — the "Keep AI" button's layout half,
 * driven through the hook itself so the ordering against the reconcile and
 * auto-grow effects is what is tested, not a helper in isolation.
 *
 * The risk being pinned: applied before the closes land, auto-grow would see
 * the doomed tabs overflow the smaller layout and grow it straight back.
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
import { useZoneLayout, type ZoneAssignments } from "./useZoneLayout";

function mount(layoutId: string, tabIds: string[], assignments: ZoneAssignments) {
  harness.reset();
  persisted.value = { layoutId, assignments, focusedZone: 0 };
  let live = tabIds;
  function ZoneLayoutHost() {
    harness.beginRender();
    return useZoneLayout(live, "compaction-test", "main");
  }
  return (next?: string[]) => {
    if (next) live = next;
    return harness.renderSettled(ZoneLayoutHost);
  };
}

describe("useZoneLayout.requestCompaction", () => {
  beforeEach(() => harness.reset());

  it("the 9-zone example: 3 AI + 3 shells + 3 empty → quad, AI in zones 0-2", () => {
    const all = ["s1", "a1", "s3", "a2", "s2", "a3"];
    const assignments = { 0: "s1", 1: "a1", 3: "s3", 4: "a2", 5: "s2", 8: "a3" };
    const render = mount("full-grid", all, assignments);
    const before = render();
    expect(before.layout.zones).toHaveLength(9);

    // The button requests first, then closes — the closes land as a new tab list.
    before.requestCompaction(["a1", "a2", "a3"], ["s1", "s2", "s3"]);
    const r = render(["a1", "a2", "a3"]);

    expect(r.layoutId).toBe("quad");
    expect(r.layout.zones).toHaveLength(4);
    expect(r.assignments).toStrictEqual({ 0: "a1", 1: "a2", 2: "a3" });
    expect(r.unassignedTabIds).toEqual([]);
  });

  it("waits while a closing tab is still in the list (no early shrink + regrow)", () => {
    const all = ["a1", "s1", "a2"];
    const render = mount("full-grid", all, { 0: "a1", 4: "s1", 8: "a2" });
    const before = render();
    before.requestCompaction(["a1", "a2"], ["s1"]);

    const still = render(all);
    expect(still.layoutId).toBe("full-grid");

    const r = render(["a1", "a2"]);
    expect(r.layoutId).toBe("split");
    expect(r.assignments).toStrictEqual({ 0: "a1", 1: "a2" });
  });

  it("a tab spawned before the closes landed is appended, never hidden", () => {
    const render = mount("full-grid", ["a1", "s1"], { 0: "s1", 5: "a1" });
    const before = render();
    before.requestCompaction(["a1"], ["s1"]);

    const r = render(["a1", "new"]);
    expect(r.layoutId).toBe("split");
    expect(r.assignments).toStrictEqual({ 0: "a1", 1: "new" });
    expect(r.unassignedTabIds).toEqual([]);
  });

  it("no AI sessions left → single empty zone", () => {
    const render = mount("quad", ["s1", "s2"], { 0: "s1", 1: "s2" });
    render().requestCompaction([], ["s1", "s2"]);
    const r = render([]);
    expect(r.layoutId).toBe("single");
    expect(r.assignments).toStrictEqual({});
  });
});

import { describe, it, expect } from "vitest";
import { classifyTabs, mountOwnerCount, type TabClassification } from "./classifyTabs";
import type { ZoneAssignments } from "./useZoneLayout";

const tabs = (...ids: string[]) => ids.map((id) => ({ id }));

/**
 * The dual-mount race is impossible iff, for every tab, exactly one classifier
 * owns it as a mount (`assigned` / `assigned-live` / `hidden`) OR zero own it
 * (`assigned-virtual`) — never two. Because `classifyTabs` returns a Map with a
 * single value per tab id, "two owners" would require a tab to be absent from
 * the map yet mounted elsewhere, or to hold two values. This asserts both the
 * per-tab owner count and total map coverage.
 */
function assertExactlyOneOrZeroOwner(
  result: Map<string, TabClassification>,
  allTabIds: string[],
) {
  for (const id of allTabIds) {
    expect(result.has(id)).toBe(true);
    const owners = mountOwnerCount(result.get(id)!);
    expect(owners === 0 || owners === 1).toBe(true);
  }
  // No tab id appears with more than one classification (Map guarantees this,
  // but assert the map has exactly one entry per unique tab).
  expect(result.size).toBe(new Set(allTabIds).size);
}

describe("classifyTabs — preset (non-flow) mode", () => {
  const assignments: ZoneAssignments = { 0: "a", 1: "b" };

  it("classifies assigned tabs 'assigned' and unassigned 'hidden'", () => {
    const result = classifyTabs(tabs("a", "b", "c"), assignments, false, new Set());
    expect(result.get("a")).toBe("assigned");
    expect(result.get("b")).toBe("assigned");
    expect(result.get("c")).toBe("hidden");
  });

  it("ignores the nearViewport set entirely in preset mode", () => {
    // Even with a fully-populated near set, preset mode never virtualizes.
    const near = new Set(["a", "b", "c"]);
    const result = classifyTabs(tabs("a", "b", "c"), assignments, false, near);
    expect(result.get("a")).toBe("assigned");
    expect(result.get("b")).toBe("assigned");
    expect(result.get("c")).toBe("hidden");
    // And with an empty near set the result is identical — proof of independence.
    const resultEmpty = classifyTabs(tabs("a", "b", "c"), assignments, false, new Set());
    expect([...result]).toEqual([...resultEmpty]);
  });

  it("upholds exactly-one-or-zero owner per tab", () => {
    const result = classifyTabs(tabs("a", "b", "c"), assignments, false, new Set());
    assertExactlyOneOrZeroOwner(result, ["a", "b", "c"]);
    // Preset mode never mounts zero for an assigned tab.
    expect(mountOwnerCount(result.get("a")!)).toBe(1);
  });
});

describe("classifyTabs — flow mode (virtualized)", () => {
  const assignments: ZoneAssignments = { 0: "a", 1: "b", 2: "c" };

  it("splits assigned into live (near) vs virtual (far), hidden unchanged", () => {
    const near = new Set(["a"]); // only 'a' is near the viewport
    const result = classifyTabs(tabs("a", "b", "c", "d"), assignments, true, near);
    expect(result.get("a")).toBe("assigned-live");
    expect(result.get("b")).toBe("assigned-virtual");
    expect(result.get("c")).toBe("assigned-virtual");
    expect(result.get("d")).toBe("hidden");
  });

  it("virtual zones mount zero instances; live/hidden mount exactly one", () => {
    const near = new Set(["a"]);
    const result = classifyTabs(tabs("a", "b", "c", "d"), assignments, true, near);
    expect(mountOwnerCount(result.get("a")!)).toBe(1); // assigned-live
    expect(mountOwnerCount(result.get("b")!)).toBe(0); // assigned-virtual
    expect(mountOwnerCount(result.get("c")!)).toBe(0); // assigned-virtual
    expect(mountOwnerCount(result.get("d")!)).toBe(1); // hidden
    assertExactlyOneOrZeroOwner(result, ["a", "b", "c", "d"]);
  });

  it("all assigned near → all live (nothing virtualized)", () => {
    const near = new Set(["a", "b", "c"]);
    const result = classifyTabs(tabs("a", "b", "c"), assignments, true, near);
    expect(result.get("a")).toBe("assigned-live");
    expect(result.get("b")).toBe("assigned-live");
    expect(result.get("c")).toBe("assigned-live");
    assertExactlyOneOrZeroOwner(result, ["a", "b", "c"]);
  });

  it("none near → all assigned virtual (max memory relief)", () => {
    const result = classifyTabs(tabs("a", "b", "c"), assignments, true, new Set());
    expect(result.get("a")).toBe("assigned-virtual");
    expect(result.get("b")).toBe("assigned-virtual");
    expect(result.get("c")).toBe("assigned-virtual");
    // Every assigned tab mounts zero instances when fully offscreen.
    for (const id of ["a", "b", "c"]) expect(mountOwnerCount(result.get(id)!)).toBe(0);
  });

  it("flipping a tab near⇄far flips live⇄virtual and never double-mounts", () => {
    const far = classifyTabs(tabs("a", "b"), assignments, true, new Set(["a"]));
    expect(far.get("b")).toBe("assigned-virtual");
    const near = classifyTabs(tabs("a", "b"), assignments, true, new Set(["a", "b"]));
    expect(near.get("b")).toBe("assigned-live");
    // Across both passes 'b' is owned by at most one mount at a time.
    assertExactlyOneOrZeroOwner(far, ["a", "b"]);
    assertExactlyOneOrZeroOwner(near, ["a", "b"]);
  });
});

/**
 * A pane replaced in place by the runner (the usage-limit account migration):
 * the replacement is spawned BEFORE the old pane closes, so for a moment both
 * are listed. The replacement must land in the old pane's zone, and a FULL
 * grid must not grow a zone for the overlap — which the old pane's close would
 * then leave empty.
 *
 * Drives the real `useZoneLayout` through the shared hooks harness, plus the
 * event reader `replacementStepFor`.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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
  REPLACEMENT_HOLD_MS,
  flowGridSlotCount,
  useZoneLayout,
  zoneDemand,
  type ZoneAssignments,
} from "./useZoneLayout";
import { replacementStepFor } from "./useTerminalReplacement";

function mount(layoutId: string, tabIds: string[], assignments: ZoneAssignments) {
  harness.reset();
  persisted.value = { layoutId, assignments, focusedZone: 0 };
  let live = tabIds;
  function ZoneLayoutHost() {
    harness.beginRender();
    return useZoneLayout(live, "replacement-test", "main");
  }
  return (next?: string[]) => {
    if (next) live = next;
    return harness.renderSettled(ZoneLayoutHost);
  };
}

const FULL_QUAD: ZoneAssignments = { 0: "a", 1: "b", 2: "c", 3: "d" };

beforeEach(() => harness.reset());
afterEach(() => vi.useRealTimers());

describe("a pane replaced in place", () => {
  it("takes the old pane's zone in a full grid, which does not grow", () => {
    const render = mount("quad", ["a", "b", "c", "d"], FULL_QUAD);
    render().beginReplacement("b");
    // The replacement is listed beside the old pane.
    let layout = render(["a", "b", "c", "d", "n"]);
    expect(layout.layoutId).toBe("quad");

    layout.replaceTab("b", "n");
    layout = render();
    expect(layout.assignments).toStrictEqual({ 0: "a", 1: "n", 2: "c", 3: "d" });
    expect(layout.layoutId).toBe("quad");

    // The old pane closes: same grid, nothing moved, no empty zone.
    layout = render(["a", "c", "d", "n"]);
    expect(layout.layoutId).toBe("quad");
    expect(layout.assignments).toStrictEqual({ 0: "a", 1: "n", 2: "c", 3: "d" });
  });

  it("without the hold, the same overlap grows the grid (the defect)", () => {
    const render = mount("quad", ["a", "b", "c", "d"], FULL_QUAD);
    const layout = render(["a", "b", "c", "d", "n"]);
    expect(layout.layout.zones.length).toBeGreaterThan(4);
  });

  it("the hold ends with the old pane, so a later spawn grows as usual", () => {
    const render = mount("quad", ["a", "b", "c", "d"], FULL_QUAD);
    render().beginReplacement("b");
    render(["a", "b", "c", "d", "n"]).replaceTab("b", "n");
    render(["a", "c", "d", "n"]);
    const layout = render(["a", "c", "d", "n", "e"]);
    expect(layout.layout.zones.length).toBeGreaterThan(4);
  });

  it("an abandoned replacement releases the hold and leaves the old pane in place", () => {
    const render = mount("quad", ["a", "b", "c", "d"], FULL_QUAD);
    render().beginReplacement("b");
    render().endReplacement("b");
    const layout = render(["a", "b", "c", "d", "e"]);
    expect(layout.assignments[1]).toBe("b");
    expect(layout.layout.zones.length).toBeGreaterThan(4);
  });

  it("a hold whose old pane never goes expires", () => {
    vi.useFakeTimers();
    const render = mount("quad", ["a", "b", "c", "d"], FULL_QUAD);
    render().beginReplacement("b");
    vi.advanceTimersByTime(REPLACEMENT_HOLD_MS);
    const layout = render(["a", "b", "c", "d", "e"]);
    expect(layout.layout.zones.length).toBeGreaterThan(4);
  });

  it("tmux: the replacement becomes the shown session", () => {
    const render = mount("quad", ["a", "b"], { 0: "a", 1: "b" });
    render().enterTmuxMode();
    expect(render().shownTabId).toBe("a");
    render().beginReplacement("a");
    render(["a", "b", "n"]).replaceTab("a", "n");
    const layout = render(["b", "n"]);
    expect(layout.shownTabId).toBe("n");
    expect(layout.gridView.assignments).toStrictEqual({ 0: "n", 1: "b" });
  });
});

describe("zoneDemand / flowGridSlotCount", () => {
  it("counts a pane being replaced and its replacement as one", () => {
    expect(zoneDemand(["a", "b", "n"], new Set(["b"]))).toBe(2);
    expect(zoneDemand(["a", "b", "n"], new Set())).toBe(3);
    // A hold for a pane no longer listed takes nothing off.
    expect(zoneDemand(["a", "n"], new Set(["b"]))).toBe(2);
  });

  it("never sizes the flow grid below the highest assigned zone", () => {
    const assignments = { 0: "a", 1: "b", 2: "c" };
    expect(flowGridSlotCount(["a", "b", "c"], assignments, new Set(["c"]))).toBe(3);
    expect(flowGridSlotCount(["a", "b", "c", "n"], assignments, new Set(["c"]))).toBe(3);
  });
});

describe("replacementStepFor", () => {
  it("reads the three steps for this page", () => {
    expect(
      replacementStepFor({ pageId: "p", oldTerminalId: "o", newTerminalId: null }, "p"),
    ).toStrictEqual({ kind: "begin", oldId: "o" });
    expect(
      replacementStepFor({ pageId: "p", oldTerminalId: "o", newTerminalId: "n" }, "p"),
    ).toStrictEqual({ kind: "complete", oldId: "o", newId: "n" });
    expect(
      replacementStepFor({ pageId: "p", oldTerminalId: "o", newTerminalId: "" }, "p"),
    ).toStrictEqual({ kind: "abandon", oldId: "o" });
  });

  it("ignores another page's event and one with no old pane", () => {
    expect(
      replacementStepFor({ pageId: "q", oldTerminalId: "o", newTerminalId: "n" }, "p"),
    ).toBeNull();
    expect(replacementStepFor({ pageId: "p", newTerminalId: "n" }, "p")).toBeNull();
    expect(replacementStepFor(null, "p")).toBeNull();
  });

  it("an event with no page belongs to the default page", () => {
    expect(replacementStepFor({ oldTerminalId: "o", newTerminalId: "n" }, "default")).toStrictEqual(
      { kind: "complete", oldId: "o", newId: "n" },
    );
  });
});
